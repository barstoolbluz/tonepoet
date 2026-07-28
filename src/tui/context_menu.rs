//! Context menu: right-click / `m` keybinding opens a floating menu
//! with context-sensitive actions. Per-screen `build_*_menu` functions
//! produce filtered item lists; `execute_context_action` dispatches.

use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};
use tokio::sync::mpsc;

use super::app::*;
use super::browse::BrowseEntry;
use crate::convert::classify::EntryKind;
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

pub(crate) const TONEPOET_CLIPBOARD_NO_TAG_BLOCKS_STATUS: &str =
    "tonepoet's clipboard has no tag blocks; paste from the system clipboard with your terminal's paste key instead";

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

/// Canonical tag groups offered by the Copy tags submenu. Matching is by
/// display key because the numbering totals intentionally use Unknown ItemKeys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagCopySelection {
    All,
    CanonicalOnly,
    Artist,
    Album,
    Title,
    Year,
    Genre,
    Performer,
    Composer,
    AlbumArtist,
    TrackNumbers,
    Comment,
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
    /// Create a file in the current browse directory.
    NewFile,
    /// Create a folder in the current browse directory.
    NewFolder,
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
    /// Put the current Browse selection on the shared filesystem clipboard.
    CutSelection,
    /// Put a copy of the current Browse selection on the shared filesystem clipboard.
    CopySelection,
    /// Paste the shared filesystem clipboard into the displayed directory.
    PasteSelection,
    /// Duplicate selected files in place.
    DuplicateSelection,
    /// Open a single file in the user's configured system editor.
    OpenSystemDefault(PathBuf),
    /// Tree-pane operations carry their target explicitly so a later cursor
    /// change cannot redirect the operation.
    TreeNewFile(PathBuf),
    TreeNewFolder(PathBuf),
    TreeRename(PathBuf),
    TreeCut(PathBuf),
    TreeCopy(PathBuf),
    TreePaste(PathBuf),
    TreeDelete(PathBuf),
    /// Address-bar text clipboard operations.
    PathTextCut,
    PathTextCopy,
    PathTextPaste,
    /// Shared active-editor operations. The concrete `TextInputState` is
    /// resolved from `AppState::editor_context_target`, so context menus never
    /// copy or reconstruct editor state.
    EditorPaste,
    EditorCopy,
    EditorCut,
    EditorDelete,
    EditorSelectAll,
    EditorFixCapitalization(crate::convert::renaming::CaseTransform),
    /// Apply a case-only rename through the existing no-clobber rename plan.
    RenameFixCapitalization(crate::convert::renaming::CaseTransform),
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
    /// Read selected metadata into the session tag clipboard.
    CopyTags(TagCopySelection),
    /// Deferred Custom tag-field builder; surfaced honestly this round.
    CopyTagsCustomDeferred,
    /// Open the Browse transfer picker with a frozen source/target side.
    BrowseTransferTags {
        direction: TagTransferDirection,
        scope: TagTransferScope,
    },
    /// Metadata editor footer popup: launch one of the tag lookup/apply flows.
    MetadataTagsMusicBrainz,
    MetadataTagsGnudb,
    MetadataTagsClipboard,
    MetadataTagsFile,
    /// Metadata editor footer popup: transfer between the open editor and a picker target.
    MetadataTransferTags {
        direction: TagTransferDirection,
        scope: TagTransferScope,
    },
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
    /// MetadataEditor row-level clipboard and selection actions.
    MetadataRowsCopy,
    MetadataRowsCut,
    MetadataRowsPaste,
    MetadataRowsSelectAll,
    MetadataRowsInvertSelection,
    MetadataRowsDeselect,
    /// MetadataEditor: open the existing per-file value browser for this row.
    MetadataEditValuesPerFile,
    /// MetadataEditor: immediately apply one numbering scheme.
    MetadataAutoNumber(
        crate::tui::metadata_autonumber::NumberingTarget,
        crate::tui::metadata_autonumber::NumberingScheme,
    ),
    /// MetadataEditor: open the custom side-prefix/preview overlay.
    MetadataOpenAutoNumber(crate::tui::metadata_autonumber::NumberingTarget),
    /// MetadataEditor: derive and populate one count/number field.
    MetadataAutoPopulate(crate::tui::metadata_autonumber::AutoPopulateTarget),
    /// MetadataEditor: open a read-only CuePreview seeded with the
    /// cursor row's value (synthetic-preview rows like CUESHEET).
    MetadataCueView,
    /// MetadataEditor: edit the active embedded/synthetic CUESHEET via the
    /// configured system editor and stage it through the metadata editor.
    MetadataCueEdit,
    /// MetadataEditor: open the destructive-action confirmation for staging
    /// deletion of the active embedded/synthetic CUESHEET.
    MetadataCueDelete,
    /// Browse: open metadata for the selected cue-bearing item and view its
    /// embedded/synthetic CUESHEET row.
    BrowseCueView,
    /// Browse: open metadata for the selected cue-bearing item and edit its
    /// embedded/synthetic CUESHEET row.
    BrowseCueEdit,
    /// Browse: open metadata for the selected cue-bearing item and stage
    /// embedded/synthetic CUESHEET deletion after confirmation.
    BrowseCueDelete,
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

fn item_enabled(label: &str, action: ContextAction, enabled: bool) -> ContextMenuEntry {
    ContextMenuEntry::Item(ContextMenuItem {
        label: label.to_string(),
        action,
        shortcut: None,
        enabled,
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
fn build_file_ops_submenu(
    include_bulk_rename: bool,
    allow_single_item_actions: bool,
    allow_duplicate: bool,
) -> ContextMenuEntry {
    let mut children = vec![
        item("Cut", ContextAction::CutSelection),
        item("Copy", ContextAction::CopySelection),
    ];
    children.push(ContextMenuEntry::Submenu {
        label: "Rename".to_string(),
        children: vec![
            item_enabled(
                "Rename",
                ContextAction::RenameEntry,
                allow_single_item_actions,
            ),
            ContextMenuEntry::Submenu {
                label: "Fix capitalization".to_string(),
                children: vec![
                    item(
                        "Title Case",
                        ContextAction::RenameFixCapitalization(
                            crate::convert::renaming::CaseTransform::Title,
                        ),
                    ),
                    item(
                        "UPPERCASE",
                        ContextAction::RenameFixCapitalization(
                            crate::convert::renaming::CaseTransform::Upper,
                        ),
                    ),
                    item(
                        "lowercase",
                        ContextAction::RenameFixCapitalization(
                            crate::convert::renaming::CaseTransform::Lower,
                        ),
                    ),
                ],
            },
        ],
    });
    if include_bulk_rename {
        children.push(item("Bulk Rename", ContextAction::BulkRename));
    }
    if allow_duplicate {
        children.push(item("Duplicate", ContextAction::DuplicateSelection));
    }
    children.push(item("Copy to...", ContextAction::CopyTo));
    children.push(item("Move to...", ContextAction::MoveTo));
    children.push(item("Delete permanently", ContextAction::DeletePermanently));
    ContextMenuEntry::Submenu {
        label: "File operations".to_string(),
        children,
    }
}

fn build_select_submenu(include_current_item: bool) -> ContextMenuEntry {
    let mut children = Vec::with_capacity(if include_current_item { 4 } else { 3 });
    if include_current_item {
        children.push(item("This item", ContextAction::Select));
    }
    children.push(item("All", ContextAction::SelectAll));
    children.push(item("Invert selection", ContextAction::SelectInverse));
    children.push(item("Deselect", ContextAction::Deselect));
    ContextMenuEntry::Submenu {
        label: "Select".to_string(),
        children,
    }
}

fn push_properties_footer(items: &mut Vec<ContextMenuEntry>) {
    if !matches!(items.last(), Some(ContextMenuEntry::Separator)) {
        items.push(separator());
    }
    items.push(item("Properties", ContextAction::EditMetadataFull));
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
    let include_properties = matches!(&entry.kind, EntryKind::AudioFile(_) | EntryKind::Directory);

    match &entry.kind {
        EntryKind::AudioFile(_) => {
            items.push(build_select_submenu(true));
            if let Some(file_ops) = archive_file_ops_submenu(entry) {
                items.push(separator());
                items.push(file_ops);
            }
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::Directory => {
            items.push(item("Open", ContextAction::OpenEntry));
            items.push(separator());
            items.push(build_select_submenu(true));
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::ParentDir => {
            items.push(item("Go up", ContextAction::OpenEntry));
            items.push(separator());
            items.push(build_select_submenu(false));
        }
        _ => {
            items.push(build_select_submenu(true));
            if let Some(file_ops) = archive_file_ops_submenu(entry) {
                items.push(separator());
                items.push(file_ops);
            }
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
    }

    if include_properties {
        push_properties_footer(&mut items);
    }
    items
}

fn archive_synthetic_file_op_status(app: &mut AppState, operation: &str) {
    app.set_status(format!(
        "archive: {operation} is unavailable inside archives; extract the file or use archive-aware metadata/rename"
    ));
}

/// Build the "Tags & Tagging" submenu (metadata editor, MusicBrainz lookup, CUE import).
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
    let copy_tags = ContextMenuEntry::Submenu {
        label: "Copy tags".to_string(),
        children: vec![
            item("All", ContextAction::CopyTags(TagCopySelection::All)),
            item(
                "Canonical Only",
                ContextAction::CopyTags(TagCopySelection::CanonicalOnly),
            ),
            separator(),
            item("Artist", ContextAction::CopyTags(TagCopySelection::Artist)),
            item("Album", ContextAction::CopyTags(TagCopySelection::Album)),
            item("Title", ContextAction::CopyTags(TagCopySelection::Title)),
            item("Year", ContextAction::CopyTags(TagCopySelection::Year)),
            item("Genre", ContextAction::CopyTags(TagCopySelection::Genre)),
            item(
                "Performer",
                ContextAction::CopyTags(TagCopySelection::Performer),
            ),
            item(
                "Composer",
                ContextAction::CopyTags(TagCopySelection::Composer),
            ),
            item(
                "Album Artist",
                ContextAction::CopyTags(TagCopySelection::AlbumArtist),
            ),
            item(
                "Track Numbers",
                ContextAction::CopyTags(TagCopySelection::TrackNumbers),
            ),
            item("Comment", ContextAction::CopyTags(TagCopySelection::Comment)),
            separator(),
            item("Custom...", ContextAction::CopyTagsCustomDeferred),
        ],
    };
    let mut children = vec![
        item("Edit metadata", ContextAction::EditMetadataFull),
        separator(),
        item("Get tags from MusicBrainz", ContextAction::TagsFromMb),
    ];
    if has_cue {
        children.push(item(
            "Get tags from CUE",
            ContextAction::ImportCueFromBrowse,
        ));
        children.push(ContextMenuEntry::Item(ContextMenuItem {
            label: "View embedded CUE sheet".to_string(),
            action: ContextAction::BrowseCueView,
            shortcut: Some(":cue-view".to_string()),
            enabled: true,
        }));
        children.push(ContextMenuEntry::Item(ContextMenuItem {
            label: "Edit embedded CUE sheet".to_string(),
            action: ContextAction::BrowseCueEdit,
            shortcut: Some(":cuesheet-edit".to_string()),
            enabled: true,
        }));
        children.push(ContextMenuEntry::Item(ContextMenuItem {
            label: "Delete embedded CUE sheet".to_string(),
            action: ContextAction::BrowseCueDelete,
            shortcut: Some(":cuesheet-delete".to_string()),
            enabled: true,
        }));
    }
    let transfer_tags = ContextMenuEntry::Submenu {
        label: "Transfer tags".to_string(),
        children: vec![
            ContextMenuEntry::Submenu {
                label: "Transfer to".to_string(),
                children: vec![
                    item(
                        "Canonical",
                        ContextAction::BrowseTransferTags {
                            direction: TagTransferDirection::To,
                            scope: TagTransferScope::Canonical,
                        },
                    ),
                    item(
                        "All",
                        ContextAction::BrowseTransferTags {
                            direction: TagTransferDirection::To,
                            scope: TagTransferScope::All,
                        },
                    ),
                ],
            },
            ContextMenuEntry::Submenu {
                label: "Transfer from".to_string(),
                children: vec![
                    item(
                        "Canonical",
                        ContextAction::BrowseTransferTags {
                            direction: TagTransferDirection::From,
                            scope: TagTransferScope::Canonical,
                        },
                    ),
                    item(
                        "All",
                        ContextAction::BrowseTransferTags {
                            direction: TagTransferDirection::From,
                            scope: TagTransferScope::All,
                        },
                    ),
                ],
            },
        ],
    };
    children.push(separator());
    children.push(copy_tags);
    children.push(transfer_tags);
    ContextMenuEntry::Submenu {
        label: "Tags & Tagging".to_string(),
        children,
    }
}

/// "Disc Tools" submenu — AccurateRip and CUETools DB lookups, both
/// of which only return useful results for redbook CD rips (16-bit /
/// 44.1 kHz with a known TOC).

/// Build the metadata editor's bottom-anchored `tags` popup. The eight leaf
/// actions are deliberately explicit so dispatch tests can pin each route.
pub(crate) fn build_metadata_tags_popup() -> Vec<ContextMenuEntry> {
    vec![
        ContextMenuEntry::Submenu {
            label: "Get tags from".to_string(),
            children: vec![
                item("MusicBrainz", ContextAction::MetadataTagsMusicBrainz),
                item("gnuDB", ContextAction::MetadataTagsGnudb),
                item("Clipboard", ContextAction::MetadataTagsClipboard),
                item("File", ContextAction::MetadataTagsFile),
            ],
        },
        ContextMenuEntry::Submenu {
            label: "Transfer tags from".to_string(),
            children: vec![
                item(
                    "Canonical",
                    ContextAction::MetadataTransferTags {
                        direction: TagTransferDirection::From,
                        scope: TagTransferScope::Canonical,
                    },
                ),
                item(
                    "All",
                    ContextAction::MetadataTransferTags {
                        direction: TagTransferDirection::From,
                        scope: TagTransferScope::All,
                    },
                ),
            ],
        },
        ContextMenuEntry::Submenu {
            label: "Transfer tags to".to_string(),
            children: vec![
                item(
                    "Canonical",
                    ContextAction::MetadataTransferTags {
                        direction: TagTransferDirection::To,
                        scope: TagTransferScope::Canonical,
                    },
                ),
                item(
                    "All",
                    ContextAction::MetadataTransferTags {
                        direction: TagTransferDirection::To,
                        scope: TagTransferScope::All,
                    },
                ),
            ],
        },
    ]
}

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

pub(crate) fn effective_browse_context_entry_kind(kind: &EntryKind, _path: &Path) -> EntryKind {
    // Context-menu construction runs on the reducer. Do not perform ISO header
    // probes here; those are filesystem reads and can stall on slow mounts.
    // Browse classification/dispatch paths remain responsible for resolving
    // concrete disc-image kind before disc-specific operations run.
    kind.clone()
}

fn audio_file_is_cue_bearing(_path: &Path) -> bool {
    // Menu construction runs on the reducer. Do not read tags or scan sibling
    // directories here: either can block on slow or disconnected mounts. Show
    // CUE actions for every audio file and let the dispatch path, which already
    // reports precise errors, resolve sidecar/embedded presence on its worker-safe path.
    true
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
    let effective_kind = effective_browse_context_entry_kind(&entry.kind, &entry.path);
    let include_properties = matches!(
        &effective_kind,
        EntryKind::AudioFile(_)
            | EntryKind::Archive
            | EntryKind::SacdIso
            | EntryKind::DvdAudioIso
            | EntryKind::DvdAudioDir
            | EntryKind::DvdVideoIso
            | EntryKind::DvdVideoDir
            | EntryKind::BlurayIso
            | EntryKind::BlurayDir
            | EntryKind::Directory
    ) || matches!(&effective_kind, EntryKind::OtherFile)
        && crate::convert::classify::is_cue_sheet_path(&entry.path);
    let selected_path_is_marked = app
        .browse
        .multi_selected
        .iter()
        .any(|path| path == &entry.path);
    let bulk_selection = selected_path_is_marked && app.browse.multi_selected.len() > 1;
    let allow_single_item_actions = !bulk_selection;
    let selection_paths: Vec<&PathBuf> = if selected_path_is_marked && !app.browse.multi_selected.is_empty() {
        app.browse.multi_selected.iter().collect()
    } else {
        vec![&entry.path]
    };
    let selection_all_files = selection_paths.iter().all(|path| {
        app.browse.entries.iter().any(|candidate| {
            candidate.path.as_path() == path.as_path()
                && !matches!(candidate.kind, EntryKind::Directory | EntryKind::ParentDir)
        })
    });

    match &effective_kind {
        EntryKind::AudioFile(_) => {
            items.push(build_convert_submenu(app));
            items.push(separator());
            items.push(build_select_submenu(true));
            items.push(separator());
            items.push(item("Analyze", ContextAction::Analyze));
            let has_cue = audio_file_is_cue_bearing(&entry.path);
            items.push(build_tagging_submenu(has_cue));
            items.push(build_disk_tools_submenu());
            items.push(build_utilities_submenu(app));
            items.push(separator());
            items.push(build_file_ops_submenu(true, allow_single_item_actions, selection_all_files));
            items.push(item(
                "Copy path",
                ContextAction::CopyPath(entry.path.clone()),
            ));
        }
        EntryKind::Archive => {
            items.push(build_convert_submenu(app));
            items.push(separator());
            items.push(build_select_submenu(true));
            items.push(separator());
            items.push(item("Set Password", ContextAction::SetArchivePassword));
            items.push(build_tagging_submenu(false));
            items.push(build_file_ops_submenu(false, allow_single_item_actions, selection_all_files));
            items.push(item(
                "Copy path",
                ContextAction::CopyPath(entry.path.clone()),
            ));
        }
        EntryKind::SacdIso => {
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
            items.push(build_select_submenu(true));
            items.push(separator());
            items.push(build_tagging_submenu(false));
            items.push(build_utilities_submenu(app));
            items.push(separator());
            items.push(build_file_ops_submenu(false, allow_single_item_actions, selection_all_files));
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
            items.push(item("Analyze", ContextAction::Analyze));
            items.push(separator());
            items.push(build_select_submenu(true));
            items.push(separator());
            items.push(build_tagging_submenu(false));
            items.push(build_utilities_submenu(app));
            items.push(separator());
            items.push(build_file_ops_submenu(false, allow_single_item_actions, selection_all_files));
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::Directory => {
            items.push(build_convert_submenu(app));
            items.push(separator());
            items.push(item("Open", ContextAction::OpenEntry));
            items.push(item("Analyze", ContextAction::Analyze));
            // Directory menu construction must not synchronously scan the
            // directory for CUE files. Show CUE-capable actions for directories
            // and let dispatch perform worker-safe presence resolution.
            items.push(build_tagging_submenu(true));
            items.push(build_disk_tools_submenu());
            items.push(build_utilities_submenu(app));
            items.push(build_select_submenu(true));
            items.push(separator());
            items.push(build_file_ops_submenu(false, allow_single_item_actions, selection_all_files));
            items.push(item(
                "Copy path",
                ContextAction::CopyPath(entry.path.clone()),
            ));
        }
        EntryKind::ParentDir => {
            items.push(item("Go up", ContextAction::OpenEntry));
            items.push(separator());
            items.push(build_select_submenu(false));
        }
        EntryKind::OtherFile => {
            // CUE files are convertible (they reference an image file).
            let is_cue = crate::convert::classify::is_cue_sheet_path(&entry.path);
            if is_cue {
                items.push(build_convert_submenu(app));
                items.push(build_tagging_submenu(true));
                items.push(separator());
            }
            if super::browse::is_viewable_text_file(&entry.path) {
                items.push(item("View", ContextAction::ViewFile(entry.path.clone())));
                if super::browse::is_editable_text_file(&entry.path) {
                    items.push(item("Edit", ContextAction::EditFile(entry.path.clone())));
                }
                items.push(separator());
            }
            items.push(build_select_submenu(true));
            items.push(separator());
            items.push(build_file_ops_submenu(false, allow_single_item_actions, selection_all_files));
            items.push(item(
                "Copy path",
                ContextAction::CopyPath(entry.path.clone()),
            ));
        }
    }

    if allow_single_item_actions && entry.path.is_file() {
        let insertion = items
            .iter()
            .rposition(|entry| {
                matches!(
                    entry,
                    ContextMenuEntry::Item(ContextMenuItem {
                        action: ContextAction::CopyPath(_),
                        ..
                    })
                )
            })
            .unwrap_or(items.len());
        items.splice(
            insertion..insertion,
            [
                separator(),
                item(
                    "Open/Edit with System Default",
                    ContextAction::OpenSystemDefault(entry.path.clone()),
                ),
            ],
        );
    }
    if include_properties {
        push_properties_footer(&mut items);
    }

    items
}


/// Build the context menu for a right-clicked Browse tree directory.
pub fn build_browse_tree_menu(app: &AppState, index: usize) -> Vec<ContextMenuEntry> {
    let Some(node) = app.browse.tree_nodes.get(index) else {
        return Vec::new();
    };
    let path = node.path.clone();
    let mutable = path.parent().is_some();
    let paste_enabled = app
        .browse
        .filesystem_clipboard
        .as_ref()
        .is_some_and(|clipboard| {
            !clipboard.is_empty() && clipboard.paths().iter().all(|source| source.exists())
        });
    vec![
        ContextMenuEntry::Submenu {
            label: "New".to_string(),
            children: vec![
                item("File", ContextAction::TreeNewFile(path.clone())),
                item("Folder", ContextAction::TreeNewFolder(path.clone())),
            ],
        },
        separator(),
        item_enabled("Cut", ContextAction::TreeCut(path.clone()), mutable),
        item("Copy", ContextAction::TreeCopy(path.clone())),
        item_enabled("Paste", ContextAction::TreePaste(path.clone()), paste_enabled),
        separator(),
        item_enabled("Rename", ContextAction::TreeRename(path.clone()), mutable),
        item_enabled("Delete permanently", ContextAction::TreeDelete(path.clone()), mutable),
        separator(),
        item("Copy path", ContextAction::CopyPath(path)),
    ]
}

/// Build the address-bar text-editing menu. The path editor is opened before
/// this builder is called, so its selection is the authoritative target.
pub fn build_browse_path_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let has_selection = app
        .browse
        .path_input
        .as_ref()
        .is_some_and(|input| input.has_selection());
    vec![
        item_enabled("Cut", ContextAction::PathTextCut, has_selection),
        item_enabled("Copy", ContextAction::PathTextCopy, has_selection),
        item("Paste", ContextAction::PathTextPaste),
    ]
}

/// Build the common context menu for every active text editor. The editor
/// remains live while this menu is open; actions operate on the exact parked
/// `TextInputState` and closing the menu restores that state unchanged.
pub fn build_editor_context_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let (has_selection, can_paste) = super::keybindings::editor_context_capabilities(app);
    vec![
        item_enabled("Paste", ContextAction::EditorPaste, can_paste),
        item_enabled("Copy", ContextAction::EditorCopy, has_selection),
        item_enabled("Cut", ContextAction::EditorCut, has_selection),
        item("Delete", ContextAction::EditorDelete),
        item("Select All", ContextAction::EditorSelectAll),
        separator(),
        ContextMenuEntry::Submenu {
            label: "Fix capitalization".to_string(),
            children: vec![
                item(
                    "Title Case",
                    ContextAction::EditorFixCapitalization(
                        crate::convert::renaming::CaseTransform::Title,
                    ),
                ),
                item(
                    "UPPERCASE",
                    ContextAction::EditorFixCapitalization(
                        crate::convert::renaming::CaseTransform::Upper,
                    ),
                ),
                item(
                    "lowercase",
                    ContextAction::EditorFixCapitalization(
                        crate::convert::renaming::CaseTransform::Lower,
                    ),
                ),
            ],
        },
    ]
}

/// Build the context menu for a right-click on empty space in the browse list.
pub fn build_browse_empty_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    vec![
        ContextMenuEntry::Submenu {
            label: "New".to_string(),
            children: vec![
                item("File", ContextAction::NewFile),
                item("Folder", ContextAction::NewFolder),
            ],
        },
        build_select_submenu(false),
        item_enabled(
            "Paste",
            ContextAction::PasteSelection,
            app.browse
                .filesystem_clipboard
                .as_ref()
                .is_some_and(|clipboard| {
                    !clipboard.is_empty() && clipboard.paths().iter().all(|path| path.exists())
                }),
        ),
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
        ContextAction::CutSelection
        | ContextAction::CopySelection
        | ContextAction::PasteSelection
        | ContextAction::DuplicateSelection => Some("filesystem clipboard operation"),
        ContextAction::OpenSystemDefault(_) => Some("system-default editing"),
        ContextAction::BulkRename
        | ContextAction::RenameFixCapitalization(_) => Some("rename"),
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
        | ContextAction::ImportCueFromBrowse
        | ContextAction::BrowseCueView
        | ContextAction::BrowseCueEdit
        | ContextAction::BrowseCueDelete => Some("CUE operations"),
        ContextAction::MarkCompareReference | ContextAction::BitCompareWithReference => {
            Some("bit comparison")
        }
        ContextAction::DetectPreemphasis => Some("pre-emphasis detection"),
        ContextAction::QueryGnudb | ContextAction::TagsFromMb => Some("tag lookup"),
        ContextAction::CopyTags(_) => Some("tag copying"),
        ContextAction::BrowseTransferTags { .. } => Some("tag transfer"),
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

fn focus_metadata_editor_on_cuesheet_row(
    state: &mut Box<super::app::MetadataEditorState>,
    operation: &str,
) -> Result<usize, String> {
    let Some(idx) = state
        .active_surface()
        .entries
        .iter()
        .position(|entry| entry.display_key.eq_ignore_ascii_case("CUESHEET"))
    else {
        return Err(format!("{operation}: selected item has no embedded CUE sheet"));
    };
    state.cursor = idx;
    Ok(idx)
}

fn open_browse_metadata_cue_action(
    app: &mut AppState,
    action: ContextAction,
    _tx: &mpsc::Sender<AppMessage>,
) {
    super::keybindings::open_metadata_editor(app);
    let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
    let mut state = match overlay {
        ActiveOverlay::MetadataEditor(state) => state,
        other => {
            app.active_overlay = other;
            app.set_status("CUE sheet action: metadata editor did not open for the selected item");
            return;
        }
    };

    let cue_idx = match focus_metadata_editor_on_cuesheet_row(&mut state, "CUE sheet action") {
        Ok(idx) => idx,
        Err(reason) => {
            app.set_status(reason);
            app.active_overlay = ActiveOverlay::MetadataEditor(state);
            return;
        }
    };

    match action {
        ContextAction::BrowseCueView => {
            let entry = &state.active_surface().entries[cue_idx];
            let content = entry
                .per_file_values
                .first()
                .cloned()
                .unwrap_or_else(|| entry.value.clone());
            let summary = format!(
                "{} (read-only · {})",
                entry.display_key,
                super::probe::cue_summary_string(&content),
            );
            app.pending_metadata_editor = Some(state);
            app.active_overlay = ActiveOverlay::CuePreview(Box::new(
                super::app::CuePreviewState::new_readonly(content, summary),
            ));
        }
        ContextAction::BrowseCueEdit => {
            let status = super::keybindings::metadata_editor_edit_embedded_cuesheet_with_system_editor(&mut state);
            app.force_redraw = true;
            app.set_status(status);
            app.active_overlay = ActiveOverlay::MetadataEditor(state);
        }
        ContextAction::BrowseCueDelete => {
            super::keybindings::open_embedded_cuesheet_delete_confirmation(app, state);
        }
        _ => {
            app.active_overlay = ActiveOverlay::MetadataEditor(state);
        }
    }
}

pub(crate) fn tag_entry_matches_copy_selection(
    entry: &super::probe::TagEntry,
    selection: TagCopySelection,
) -> bool {
    if selection == TagCopySelection::All {
        return true;
    }
    let key = entry.display_key.to_ascii_uppercase();
    match selection {
        TagCopySelection::All => true,
        TagCopySelection::CanonicalOnly => matches!(
            key.as_str(),
            "TITLE"
                | "ARTIST"
                | "ALBUM"
                | "DATE"
                | "GENRE"
                | "PERFORMER"
                | "COMPOSER"
                | "ALBUMARTIST"
                | "TRACKNUMBER"
                | "TRACKTOTAL"
                | "DISCNUMBER"
                | "DISCTOTAL"
                | "COMMENT"
        ),
        TagCopySelection::Artist => key == "ARTIST",
        TagCopySelection::Album => key == "ALBUM",
        TagCopySelection::Title => key == "TITLE",
        TagCopySelection::Year => key == "DATE",
        TagCopySelection::Genre => key == "GENRE",
        TagCopySelection::Performer => key == "PERFORMER",
        TagCopySelection::Composer => key == "COMPOSER",
        TagCopySelection::AlbumArtist => key == "ALBUMARTIST",
        TagCopySelection::TrackNumbers => matches!(
            key.as_str(),
            "TRACKNUMBER" | "TRACKTOTAL" | "DISCNUMBER" | "DISCTOTAL"
        ),
        TagCopySelection::Comment => key == "COMMENT",
    }
}

pub(crate) const TAG_CLIPBOARD_COPY_MAX_VISITED: usize = 100_000;
pub(crate) const TAG_CLIPBOARD_COPY_MAX_AUDIO_FILES: usize = 50_000;

fn start_tag_clipboard_copy(
    app: &mut AppState,
    selection: TagCopySelection,
    tx: &mpsc::Sender<AppMessage>,
) {
    let roots = super::command::current_bulk_guard_paths(app);
    if roots.is_empty() {
        app.set_status("Copy tags: no files selected");
        return;
    }

    let generation = app
        .browse
        .tag_clipboard_copy_generation
        .checked_add(1)
        .unwrap_or(1);
    app.browse.tag_clipboard_copy_generation = generation;
    let request = super::browse::PendingTagClipboardCopy {
        generation,
        roots,
        selection,
    };

    if let Some(cancel) = app.browse.tag_clipboard_copy_cancel.as_ref() {
        // `spawn_blocking` cannot be force-aborted once scheduled. Cooperatively
        // stop the current walk/read at its next cancellation point and retain
        // only the latest request. This bounds resource use to one active plus
        // one coalesced pending operation regardless of click frequency.
        cancel.store(true, Ordering::Release);
        app.browse.tag_clipboard_copy_pending = Some(request);
        app.set_status("Copying tags...");
        return;
    }

    launch_tag_clipboard_copy(app, request, tx);
}

fn launch_tag_clipboard_copy(
    app: &mut AppState,
    request: super::browse::PendingTagClipboardCopy,
    tx: &mpsc::Sender<AppMessage>,
) {
    debug_assert!(app.browse.tag_clipboard_copy_cancel.is_none());
    debug_assert!(app.browse.tag_clipboard_copy_active_generation.is_none());

    let super::browse::PendingTagClipboardCopy {
        generation,
        roots,
        selection,
    } = request;
    let cancel = Arc::new(AtomicBool::new(false));
    app.browse.tag_clipboard_copy_active_generation = Some(generation);
    app.browse.tag_clipboard_copy_cancel = Some(cancel.clone());
    app.set_status("Copying tags...");

    let worker_tx = tx.clone();
    tokio::spawn(async move {
        let worker_cancel = cancel.clone();
        let completion = tokio::task::spawn_blocking(move || {
            // Preserve metadata-expansion semantics while adding both a hard
            // traversal bound and cooperative cancellation. No stale request
            // may continue recursively reading an arbitrarily large tree.
            let source_paths = match super::command::expand_audio_paths_for_metadata_limited(
                &roots,
                TAG_CLIPBOARD_COPY_MAX_VISITED,
                TAG_CLIPBOARD_COPY_MAX_AUDIO_FILES,
                || worker_cancel.load(Ordering::Acquire),
            ) {
                Ok(paths) => paths,
                Err(reason) => return (Vec::new(), Err(reason)),
            };

            if worker_cancel.load(Ordering::Acquire) {
                return (
                    Vec::new(),
                    Err("Copy tags superseded by a newer request".to_string()),
                );
            }
            if source_paths.is_empty() {
                return (
                    source_paths,
                    Err("Copy tags: selection contains no audio files".to_string()),
                );
            }

            let result = super::probe::read_all_tags_merged_with_metadata_cancellable(
                &source_paths,
                || worker_cancel.load(Ordering::Acquire),
            )
            .and_then(|merged| {
                let failure_count = merged
                    .metadata_errors
                    .iter()
                    .filter(|issue| issue.is_some())
                    .count();
                if failure_count == source_paths.len() {
                    let detail = merged
                        .metadata_errors
                        .iter()
                        .flatten()
                        .map(|issue| issue.reason.as_str())
                        .next()
                        .unwrap_or("all selected files failed metadata reading");
                    return Err(format!("Copy tags failed: {detail}"));
                }
                let entries = merged
                    .entries
                    .iter()
                    .filter(|entry| tag_entry_matches_copy_selection(entry, selection))
                    .cloned()
                    .collect();
                Ok((entries, failure_count))
            });
            (source_paths, result)
        })
        .await;

        let (source_paths, result) = match completion {
            Ok(completion) => completion,
            Err(err) => (
                Vec::new(),
                Err(format!("Copy tags failed: background worker did not complete: {err}")),
            ),
        };
        let _ = worker_tx
            .send(AppMessage::TagClipboardCopyComplete {
                generation,
                source_paths,
                result,
            })
            .await;
    });
}

pub(crate) fn handle_tag_clipboard_copy_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    generation: u64,
    source_paths: Vec<PathBuf>,
    result: Result<(Vec<crate::tui::probe::TagEntry>, usize), String>,
) {
    handle_tag_clipboard_copy_complete_with_publisher(
        app,
        tx,
        generation,
        source_paths,
        result,
        publish_text_clipboard,
    );
}

fn handle_tag_clipboard_copy_complete_with_publisher<F>(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    generation: u64,
    source_paths: Vec<PathBuf>,
    result: Result<(Vec<crate::tui::probe::TagEntry>, usize), String>,
    mut publish: F,
) where
    F: FnMut(&str),
{
    // A completion may mutate worker ownership only when it belongs to the
    // currently active worker. This also makes duplicate/stray completions
    // harmless.
    if app.browse.tag_clipboard_copy_active_generation != Some(generation) {
        return;
    }
    app.browse.tag_clipboard_copy_active_generation = None;
    app.browse.tag_clipboard_copy_cancel = None;

    // Publish only the newest request. Superseded completions exist solely to
    // drain the single worker slot and launch the latest coalesced request.
    if generation == app.browse.tag_clipboard_copy_generation
        && app.browse.tag_clipboard_copy_pending.is_none()
    {
        match result {
            Ok((entries, failure_count)) => {
                let file_count = source_paths.len();
                let serialized = super::tag_interchange::serialize_tag_entries(entries.iter());
                let field_count = serialized.keys.len();
                publish(&serialized.text);
                app.browse.tag_clipboard = Some(super::browse::TagClipboard {
                    source_paths,
                    entries,
                });
                let mut status = format!(
                    "Copied {} field{} from {} file{} (text clipboard)",
                    field_count,
                    if field_count == 1 { "" } else { "s" },
                    file_count,
                    if file_count == 1 { "" } else { "s" },
                );
                if failure_count > 0 {
                    status.push_str(&format!(
                        "; {} file{} could not be read",
                        failure_count,
                        if failure_count == 1 { "" } else { "s" },
                    ));
                }
                for skipped in &serialized.skipped {
                    status.push_str(&format!("; {} skipped: {}", skipped.key, skipped.reason));
                }
                app.set_status(status);
            }
            Err(reason) => app.set_status(reason),
        }
    }

    if let Some(pending) = app.browse.tag_clipboard_copy_pending.take() {
        launch_tag_clipboard_copy(app, pending, tx);
    }
}

fn open_browse_tag_transfer_picker(
    app: &mut AppState,
    direction: TagTransferDirection,
    scope: TagTransferScope,
) {
    let fixed_roots = super::command::current_bulk_guard_paths(app);
    if fixed_roots.is_empty() {
        app.set_status("Transfer tags: no files selected");
        return;
    }
    let start_dir = if app.browse.current_dir.is_dir() {
        app.browse.current_dir.clone()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    let title = match direction {
        TagTransferDirection::To => "Transfer tags to...",
        TagTransferDirection::From => "Transfer tags from...",
    };
    let picker = tui_file_picker::FilePickerState::new(
        super::keybindings::file_picker_config_with_browse_sort(
            app,
            tui_file_picker::FilePickerConfig {
                start_dir,
                filter: tui_file_picker::FilePickerFilter::Audio,
                title: title.to_string(),
                theme: super::keybindings::file_picker_theme_from_theme(&app.theme),
                selection_mode: tui_file_picker::FilePickerSelectionMode::FilesOrDirectories,
                operation_policy: tui_file_picker::FileOperationPolicy {
                    allow_new_file: false,
                    allow_new_folder: false,
                    allow_cut: false,
                    allow_copy: false,
                    allow_paste: false,
                    allow_delete: false,
                    ..tui_file_picker::FileOperationPolicy::default()
                },
                ..tui_file_picker::FilePickerConfig::default()
            },
        ),
    );
    app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(
        FilePickerPurpose::BrowseTagTransfer {
            direction,
            scope,
            fixed_roots,
        },
        picker,
    ));
    app.set_status(format!("{title}: choose one audio file or directory"));
}

pub(crate) fn start_tag_transfer(
    app: &mut AppState,
    source_roots: Vec<PathBuf>,
    target_roots: Vec<PathBuf>,
    scope: TagTransferScope,
    tx: &mpsc::Sender<AppMessage>,
) {
    if source_roots.is_empty() || target_roots.is_empty() {
        app.set_status("Transfer tags: source and target are required");
        return;
    }
    let generation = app.browse.tag_transfer_generation.checked_add(1).unwrap_or(1);
    app.browse.tag_transfer_generation = generation;
    let request = super::browse::PendingTagTransfer {
        generation,
        source: super::browse::PendingTagTransferSource::Roots(source_roots),
        target_roots,
        scope,
        verification: app.config.file_operations.verification,
    };

    if let Some(cancel) = app.browse.tag_transfer_cancel.as_ref() {
        cancel.cancel();
        app.browse.tag_transfer_pending = Some(request);
        app.set_status("Transfer tags: cancelling superseded operation...");
        return;
    }
    launch_tag_transfer(app, request, tx);
}

pub(crate) fn start_tag_transfer_from_editor_snapshot(
    app: &mut AppState,
    entries: Vec<super::probe::TagEntry>,
    source_count: usize,
    target_roots: Vec<PathBuf>,
    scope: TagTransferScope,
    tx: &mpsc::Sender<AppMessage>,
) {
    if source_count == 0 || target_roots.is_empty() {
        app.set_status("Transfer tags: editor source and target are required");
        return;
    }
    let generation = app.browse.tag_transfer_generation.checked_add(1).unwrap_or(1);
    app.browse.tag_transfer_generation = generation;
    let request = super::browse::PendingTagTransfer {
        generation,
        source: super::browse::PendingTagTransferSource::EditorSnapshot {
            entries,
            source_count,
        },
        target_roots,
        scope,
        verification: app.config.file_operations.verification,
    };
    if let Some(cancel) = app.browse.tag_transfer_cancel.as_ref() {
        cancel.cancel();
        app.browse.tag_transfer_pending = Some(request);
        app.set_status("Transfer tags: cancelling superseded operation...");
        return;
    }
    launch_tag_transfer(app, request, tx);
}

fn launch_tag_transfer(
    app: &mut AppState,
    request: super::browse::PendingTagTransfer,
    tx: &mpsc::Sender<AppMessage>,
) {
    debug_assert!(app.browse.tag_transfer_cancel.is_none());
    debug_assert!(app.browse.tag_transfer_active_generation.is_none());
    let super::browse::PendingTagTransfer {
        generation,
        source,
        target_roots,
        scope,
        verification,
    } = request;
    let cancel = super::probe::MetadataWriteCancelFlag::new();
    app.browse.tag_transfer_active_generation = Some(generation);
    app.browse.tag_transfer_cancel = Some(cancel.clone());
    app.set_status("Transfer tags: reading source and target metadata...");

    let worker_tx = tx.clone();
    tokio::spawn(async move {
        let worker_cancel = cancel.clone();
        let progress_tx = worker_tx.clone();
        let completion = tokio::task::spawn_blocking(move || {
            let progress = |completed: usize, total: usize, path: &std::path::Path| {
                // File-count progress is advisory. Never block a metadata write
                // behind a saturated UI channel, and avoid message floods for
                // large directory transfers.
                if completed == 1 || completed == total || completed % 16 == 0 {
                    let _ = progress_tx.try_send(AppMessage::TagTransferProgress {
                        generation,
                        completed,
                        total,
                        path: path.to_path_buf(),
                    });
                }
            };
            let target_paths = super::command::expand_audio_paths_for_metadata_limited(
                &target_roots,
                TAG_CLIPBOARD_COPY_MAX_VISITED,
                TAG_CLIPBOARD_COPY_MAX_AUDIO_FILES,
                || worker_cancel.is_cancelled(),
            )?;
            if target_paths.is_empty() {
                return Err("Transfer tags: target contains no audio files".to_string());
            }
            match source {
                super::browse::PendingTagTransferSource::Roots(source_roots) => {
                    let source_paths = super::command::expand_audio_paths_for_metadata_limited(
                        &source_roots,
                        TAG_CLIPBOARD_COPY_MAX_VISITED,
                        TAG_CLIPBOARD_COPY_MAX_AUDIO_FILES,
                        || worker_cancel.is_cancelled(),
                    )?;
                    if source_paths.is_empty() {
                        return Err("Transfer tags: source contains no audio files".to_string());
                    }
                    super::tag_interchange::execute_tag_transfer_from_paths(
                        &source_paths,
                        &target_paths,
                        scope,
                        verification,
                        &worker_cancel,
                        Some(&progress),
                    )
                }
                super::browse::PendingTagTransferSource::EditorSnapshot {
                    entries,
                    source_count,
                } => super::tag_interchange::execute_tag_transfer_from_entries(
                    &entries,
                    source_count,
                    &target_paths,
                    scope,
                    verification,
                    &worker_cancel,
                    Some(&progress),
                ),
            }
        })
        .await
        .unwrap_or_else(|err| Err(format!("Transfer tags worker failed: {err}")));
        let _ = worker_tx
            .send(AppMessage::TagTransferComplete {
                generation,
                result: completion,
            })
            .await;
    });
}

pub(crate) fn handle_tag_transfer_progress(
    app: &mut AppState,
    generation: u64,
    completed: usize,
    total: usize,
    path: &std::path::Path,
) {
    if app.browse.tag_transfer_active_generation != Some(generation)
        || app.browse.tag_transfer_generation != generation
        || app.browse.tag_transfer_pending.is_some()
    {
        return;
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("target");
    app.set_status(format!(
        "Transfer tags: {completed} of {total} files processed ({name})"
    ));
}

fn record_tag_transfer_report_side_effects(
    app: &mut AppState,
    report: &super::tag_interchange::TagTransferReport,
) {
    for path in &report.written_paths {
        app.browse.remove_probe_cache_entry(path);
        app.browse.probe_pending.remove(path);
        app.browse.invalidate_search_tag_cache_for_metadata_path(path);
        let _ = app.db.invalidate_probe(&path.display().to_string());
    }
    if !report.written_paths.is_empty() {
        app.browse.tag_transfer_refresh_pending = true;
    }
    for (path, reason) in &report.failed {
        log::error!("tag transfer failed for '{}': {}", path.display(), reason);
    }
    for warning in &report.cardinality_warnings {
        log::warn!("tag transfer: {warning}");
    }
    for warning in &report.durability_warnings {
        log::warn!("tag transfer durability: {warning}");
    }
}

pub(crate) fn handle_tag_transfer_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    generation: u64,
    result: Result<super::tag_interchange::TagTransferReport, String>,
) {
    if app.browse.tag_transfer_active_generation != Some(generation) {
        return;
    }
    app.browse.tag_transfer_active_generation = None;
    app.browse.tag_transfer_cancel = None;

    // Unlike Copy tags, a superseded transfer may already have committed one
    // or more target files before cancellation was observed. Always account
    // for those side effects, even when an older completion no longer owns the
    // status line. Otherwise the probe/search caches could indefinitely expose
    // pre-transfer metadata for files that were in fact changed.
    if let Ok(report) = &result {
        record_tag_transfer_report_side_effects(app, report);
    }

    let owns_status = generation == app.browse.tag_transfer_generation
        && app.browse.tag_transfer_pending.is_none();
    if owns_status {
        match &result {
            Ok(report) => app.set_status(report.status()),
            Err(reason) => app.set_status(reason.clone()),
        }
    } else if let Err(reason) = &result {
        log::debug!("superseded tag transfer completed without a report: {reason}");
    }

    if let Some(pending) = app.browse.tag_transfer_pending.take() {
        launch_tag_transfer(app, pending, tx);
        return;
    }

    if app.browse.tag_transfer_refresh_pending {
        app.browse.tag_transfer_refresh_pending = false;
        app.browse.probe_current_with_db(tx, Some(&app.db));
    }
}

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
        ContextAction::BrowseCueView | ContextAction::BrowseCueEdit | ContextAction::BrowseCueDelete => {
            open_browse_metadata_cue_action(app, action, tx);
        }
        // ── Browse: Convert actions ─────────────────────────────────
        ContextAction::ConvertCustom => {
            // Open Convert screen for manual review — no auto-commit.
            let cmd = super::command::Command::Queue { preset: None };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::ConvertLastUsed => {
            let start = resolve_convert_start(app, invert);
            super::command::execute_queue_with_post_load_commit(app, tx, None, start);
        }
        ContextAction::ConvertWithPreset(name) => {
            let start = resolve_convert_start(app, invert);
            super::command::execute_queue_with_post_load_commit(app, tx, Some(name), start);
        }
        ContextAction::Select => {
            if app.current_screen == AppScreen::Browse {
                app.browse.toggle_selection();
                app.cancel_browse_convert_expansion_for_browse_change("browse selection changed");
            }
        }
        ContextAction::SelectAll => {
            if app.current_screen == AppScreen::Browse {
                use crate::convert::classify::EntryKind;
                let paths: Vec<std::path::PathBuf> = app
                    .browse
                    .entries
                    .iter()
                    .filter(|e| !matches!(e.kind, EntryKind::ParentDir))
                    .map(|e| e.path.clone())
                    .collect();
                app.browse.multi_selected = paths;
                app.cancel_browse_convert_expansion_for_browse_change("browse selection changed");
            }
        }
        ContextAction::SelectInverse => {
            if app.current_screen == AppScreen::Browse {
                use crate::convert::classify::EntryKind;
                let new_sel: Vec<std::path::PathBuf> = app
                    .browse
                    .entries
                    .iter()
                    .filter(|e| !matches!(e.kind, EntryKind::ParentDir))
                    .filter(|e| !app.browse.multi_selected.iter().any(|p| *p == e.path))
                    .map(|e| e.path.clone())
                    .collect();
                app.browse.multi_selected = new_sel;
                app.cancel_browse_convert_expansion_for_browse_change("browse selection changed");
            }
        }
        ContextAction::Deselect => {
            if app.current_screen == AppScreen::Browse {
                app.browse.clear_multi_selection();
                app.cancel_browse_convert_expansion_for_browse_change("browse selection changed");
            }
        }
        ContextAction::OpenEntry => {
            if app.current_screen == AppScreen::Browse {
                let index = app.browse.selected_index;
                super::keybindings::activate_browse_entry(app, index, tx);
            }
        }
        ContextAction::NewFile => {
            super::keybindings::begin_browse_create(app, super::app::BrowseCreateKind::File);
        }
        ContextAction::NewFolder => {
            super::keybindings::begin_browse_create(app, super::app::BrowseCreateKind::Folder);
        }
        ContextAction::TreeNewFile(path) => {
            super::keybindings::begin_browse_create_in(
                app,
                path,
                super::app::BrowseCreateKind::File,
            );
        }
        ContextAction::TreeNewFolder(path) => {
            super::keybindings::begin_browse_create_in(
                app,
                path,
                super::app::BrowseCreateKind::Folder,
            );
        }
        ContextAction::TreeRename(path) => {
            super::keybindings::begin_browse_inline_rename(app, path);
        }
        ContextAction::TreeCut(path) => {
            if let Some(clipboard) = tui_file_picker::FilesystemClipboard::new(
                tui_file_picker::FilePickerClipboardMode::Cut,
                vec![path],
            ) {
                app.browse.filesystem_clipboard = Some(clipboard);
                app.browse.filesystem_clipboard_retry_plan = None;
                app.set_status("Cut tree folder");
            }
        }
        ContextAction::TreeCopy(path) => {
            if let Some(clipboard) = tui_file_picker::FilesystemClipboard::new(
                tui_file_picker::FilePickerClipboardMode::Copy,
                vec![path],
            ) {
                app.browse.filesystem_clipboard = Some(clipboard);
                app.browse.filesystem_clipboard_retry_plan = None;
                app.set_status("Copied tree folder");
            }
        }
        ContextAction::TreePaste(target_dir) => {
            let Some(clipboard) = app.browse.filesystem_clipboard.clone() else {
                app.set_status("Nothing to paste");
                return;
            };
            super::keybindings::start_filesystem_clipboard_paste(
                app,
                clipboard,
                target_dir,
                tx,
            );
        }
        ContextAction::TreeDelete(path) => {
            app.active_overlay = ActiveOverlay::Confirmation {
                message: format!(
                    "Permanently delete this folder?\n\n{}\n\nThis cannot be undone.",
                    path.display()
                ),
                action: ConfirmAction::DeleteSelection(vec![path]),
            };
        }
        ContextAction::PathTextCut => {
            if let Some(input) = app.browse.path_input.as_mut() {
                input.cut_selection();
            }
        }
        ContextAction::PathTextCopy => {
            if let Some(input) = app.browse.path_input.as_mut() {
                input.copy_selection();
            }
        }
        ContextAction::PathTextPaste => {
            if let Some(input) = app.browse.path_input.as_mut() {
                input.paste_clipboard();
            }
        }
        ContextAction::EditorPaste => {
            super::keybindings::execute_editor_context_action(app, ContextAction::EditorPaste);
        }
        ContextAction::EditorCopy => {
            super::keybindings::execute_editor_context_action(app, ContextAction::EditorCopy);
        }
        ContextAction::EditorCut => {
            super::keybindings::execute_editor_context_action(app, ContextAction::EditorCut);
        }
        ContextAction::EditorDelete => {
            super::keybindings::execute_editor_context_action(app, ContextAction::EditorDelete);
        }
        ContextAction::EditorSelectAll => {
            super::keybindings::execute_editor_context_action(app, ContextAction::EditorSelectAll);
        }
        ContextAction::EditorFixCapitalization(transform) => {
            super::keybindings::execute_editor_context_action(
                app,
                ContextAction::EditorFixCapitalization(transform),
            );
        }
        ContextAction::RenameFixCapitalization(transform) => {
            let paths = app.browse_context_action_paths.clone().or_else(|| {
                app.browse
                    .selected_entry()
                    .map(|entry| vec![entry.path.clone()])
            });
            let Some(paths) = paths else {
                app.set_status("Nothing to rename");
                return;
            };
            super::keybindings::rename_paths_with_case_transform(app, paths, transform, tx);
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
            super::keybindings::open_metadata_editor_with_tx(app, tx);
        }
        ContextAction::DeletePermanently => {
            // Browse-archive deletion is handled above by
            // archive_context_dispatch_for_action, because archive entries are
            // staged edits rather than direct filesystem paths.
            let cmd = super::command::Command::Delete;
            super::command::execute_command(app, cmd, tx);
        }
        cut_or_copy @ (ContextAction::CutSelection | ContextAction::CopySelection) => {
            if app.browse.is_in_archive() {
                archive_synthetic_file_op_status(app, "filesystem clipboard operation");
                return;
            }
            let selection = super::command::collect_selection_for_file_ops_scoped(app);
            let mode = if matches!(cut_or_copy, ContextAction::CutSelection) {
                tui_file_picker::FilePickerClipboardMode::Cut
            } else {
                tui_file_picker::FilePickerClipboardMode::Copy
            };
            match tui_file_picker::FilesystemClipboard::new(mode, selection.paths) {
                Some(clipboard) => {
                    let count = clipboard.paths().len();
                    app.browse.filesystem_clipboard = Some(clipboard);
                    app.browse.filesystem_clipboard_retry_plan = None;
                    app.set_status(format!(
                        "{} {count} item{}",
                        if mode == tui_file_picker::FilePickerClipboardMode::Cut {
                            "Cut"
                        } else {
                            "Copied"
                        },
                        if count == 1 { "" } else { "s" }
                    ));
                }
                None => app.set_status("No items selected"),
            }
            super::command::surface_stale_selection_notice(app, selection.dropped_stale_count);
        }
        ContextAction::PasteSelection => {
            if app.browse.is_in_archive() {
                archive_synthetic_file_op_status(app, "paste");
                return;
            }
            let Some(clipboard) = app.browse.filesystem_clipboard.clone() else {
                app.set_status("Nothing to paste");
                return;
            };
            super::keybindings::start_filesystem_clipboard_paste(
                app,
                clipboard,
                app.browse.current_dir.clone(),
                tx,
            );
        }
        ContextAction::DuplicateSelection => {
            if app.browse.is_in_archive() {
                archive_synthetic_file_op_status(app, "duplicate");
                return;
            }
            let selection = super::command::collect_selection_for_file_ops_scoped(app);
            match tui_file_picker::duplicate_files_in_place(
                &selection.paths,
                tui_file_picker::FileOperationPolicy::default(),
            ) {
                Ok(created) => {
                    app.browse.refresh_with_search(Some(tx));
                    app.browse.multi_selected = created.clone();
                    if let Some(last) = created.last() {
                        if let Some(index) = app.browse.entries.iter().position(|entry| &entry.path == last) {
                            app.browse.selected_index = index;
                            app.browse.ensure_visible();
                        }
                    }
                    app.set_status(format!(
                        "Duplicated {} file{}",
                        created.len(),
                        if created.len() == 1 { "" } else { "s" }
                    ));
                }
                Err(err) => app.set_status(format!("Duplicate failed: {}", err.message())),
            }
            super::command::surface_stale_selection_notice(app, selection.dropped_stale_count);
        }
        ContextAction::OpenSystemDefault(path) => {
            if !path.is_file() {
                app.set_status("Open/Edit with System Default is available for files only");
                return;
            }
            match super::external_editor::open_in_editor(&path) {
                Ok(true) => app.set_status(format!("Closed editor: {}", path.display())),
                Ok(false) => app.set_status(format!("Editor exited unsuccessfully: {}", path.display())),
                Err(err) => app.set_status(format!("Could not open {}: {err}", path.display())),
            }
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
        ContextAction::CopyTags(selection) => {
            start_tag_clipboard_copy(app, selection, tx);
        }
        ContextAction::CopyTagsCustomDeferred => {
            app.set_status("Custom tag selection arrives in a later round");
        }
        ContextAction::BrowseTransferTags { direction, scope } => {
            open_browse_tag_transfer_picker(app, direction, scope);
        }
        ContextAction::MetadataTagsMusicBrainz => {
            if let Some(state) = app.pending_metadata_editor.take() {
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
                let command = super::command::Command::TagsFromMb {
                    query: None,
                    catno: None,
                    year: None,
                };
                super::command::execute_command(app, command, tx);
            } else {
                app.set_status("metadata editor: tag popup lost its editor session");
            }
        }
        ContextAction::MetadataTagsGnudb => {
            if let Some(state) = app.pending_metadata_editor.take() {
                // GNUDB's guarded reducer requires an editor-originated lookup
                // to own a parked editor while no interactive overlay is live.
                // Freeze the editor's exact file set into the existing browse
                // selection seam before dispatch; never fall back to whatever
                // happens to be selected behind the editor.
                app.browse_context_action_paths =
                    Some(state.active_surface().paths.clone());
                app.pending_metadata_editor = Some(state);
                app.active_overlay = ActiveOverlay::None;
                execute_gnudb_query(app, tx);
            } else {
                app.set_status("metadata editor: tag popup lost its editor session");
            }
        }
        ContextAction::MetadataTagsClipboard => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status("metadata editor: tag popup lost its editor session");
                return;
            };
            let clipboard = tui_file_picker::read_shared_text_clipboard();
            let result = super::tag_interchange::parse_field_blocks(&clipboard);
            match result {
                Ok(blocks) => match super::tag_interchange::apply_field_blocks_to_editor(
                    &mut state,
                    &blocks,
                ) {
                    Ok(report) => app.set_status(
                        report.success_status(state.active_surface().paths.len()),
                    ),
                    Err(reason) => app.set_status(format!("tag blocks: {reason}")),
                },
                Err(error) => {
                    log::debug!("tonepoet clipboard did not contain applicable tag blocks: {error}");
                    app.set_status(TONEPOET_CLIPBOARD_NO_TAG_BLOCKS_STATUS);
                }
            }
            app.active_overlay = ActiveOverlay::MetadataEditor(state);
        }
        ContextAction::MetadataTagsFile => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status("metadata editor: tag popup lost its editor session");
                return;
            };
            super::keybindings::metadata_editor_open_tag_blocks_file_picker(app, &mut state);
            app.active_overlay = ActiveOverlay::MetadataEditor(state);
            app.set_status("metadata editor: choose a tag-block text file");
        }
        ContextAction::MetadataTransferTags { direction, scope } => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status("metadata editor: tag popup lost its editor session");
                return;
            };
            super::keybindings::metadata_editor_open_tag_transfer_picker(
                app,
                &mut state,
                direction,
                scope,
            );
            app.active_overlay = ActiveOverlay::MetadataEditor(state);
            app.set_status(match direction {
                TagTransferDirection::From => "metadata editor: choose a tag source",
                TagTransferDirection::To => "metadata editor: choose a tag-transfer target",
            });
        }
        ContextAction::MetadataRowsCopy => {
            if let Some(state) = app.pending_metadata_editor.take() {
                if let Err(reason) =
                    super::keybindings::metadata_editor_copy_selected_rows(app, &state)
                {
                    app.set_status(reason);
                }
                app.pending_metadata_editor = Some(state);
            } else {
                app.set_status("metadata editor: row clipboard action lost its editor session");
            }
        }
        ContextAction::MetadataRowsCut => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                if let Err(reason) =
                    super::keybindings::metadata_editor_cut_selected_rows(app, &mut state)
                {
                    app.set_status(reason);
                }
                app.pending_metadata_editor = Some(state);
            } else {
                app.set_status("metadata editor: row clipboard action lost its editor session");
            }
        }
        ContextAction::MetadataRowsPaste => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                if let Err(reason) =
                    super::keybindings::metadata_editor_paste_shared_clipboard(app, &mut state)
                {
                    app.set_status(reason);
                }
                app.pending_metadata_editor = Some(state);
            } else {
                app.set_status("metadata editor: row clipboard action lost its editor session");
            }
        }
        ContextAction::MetadataRowsSelectAll => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let count = super::keybindings::metadata_editor_select_all_rows(&mut state);
                app.set_status(format!(
                    "metadata editor: selected {} row{}",
                    count,
                    if count == 1 { "" } else { "s" },
                ));
                app.pending_metadata_editor = Some(state);
            }
        }
        ContextAction::MetadataRowsInvertSelection => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let count = super::keybindings::metadata_editor_invert_row_selection(&mut state);
                app.set_status(format!(
                    "metadata editor: {} row{} selected",
                    count,
                    if count == 1 { "" } else { "s" },
                ));
                app.pending_metadata_editor = Some(state);
            }
        }
        ContextAction::MetadataRowsDeselect => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                super::keybindings::metadata_editor_deselect_all_rows(&mut state);
                app.set_status("metadata editor: row selection cleared");
                app.pending_metadata_editor = Some(state);
            }
        }
        ContextAction::MetadataEditValuesPerFile => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let cursor = state.cursor;
                if let Some(reason) =
                    super::keybindings::metadata_editor_begin_detail_edit_for_entry(
                        &mut state,
                        cursor,
                        false,
                    )
                {
                    app.set_status(reason);
                }
                app.pending_metadata_editor = Some(state);
            }
        }
        ContextAction::MetadataRevertMb => {
            // Toggle the parked editor's cursor row, mirroring :revert.
            // The parking restore logic in `run_context_action_restoring_parked`
            // puts the editor back as the active overlay after this returns.
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let cursor = state.cursor;
                if let Some(entry) = state.active_surface_mut().entries.get_mut(cursor) {
                    let mutation_report = super::probe::toggle_mb_revert(entry);
                    let mut status = match super::probe::mb_pill_state(entry) {
                        super::probe::MbRevertPill::Revert => format!(
                            "MusicBrainz values applied to {}",
                            entry.display_key
                        ),
                        super::probe::MbRevertPill::UseMb => {
                            format!("{} reverted to file values", entry.display_key)
                        }
                        super::probe::MbRevertPill::None => {
                            format!("{} unchanged", entry.display_key)
                        }
                    };
                    mutation_report.append_collapse_warning(&mut status);
                    state.recompute_active_dirty();
                    app.set_status(status);
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataEditValue => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                if let Some(status) = super::keybindings::metadata_editor_begin_cursor_value_edit(&mut state, false) {
                    app.set_status(status);
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataDeleteEntry => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                if super::keybindings::metadata_editor_delete_cursor_requires_embedded_cuesheet_confirmation(&state) {
                    super::keybindings::open_embedded_cuesheet_delete_confirmation(app, state);
                } else {
                    if let Some(status) = super::keybindings::metadata_editor_delete_cursor(&mut state) {
                        app.set_status(status);
                    }
                    app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                }
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
                if let Some(status) = super::keybindings::metadata_editor_open_add(&mut state) {
                    app.set_status(status);
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataAutoNumber(target, scheme) => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status("Auto-number: owning metadata editor is no longer available");
                return;
            };
            match super::metadata_autonumber::apply_numbering(
                &mut state,
                target,
                scheme,
                None,
            ) {
                Ok(report) => app.set_status(report.status("Auto-numbered")),
                Err(error) => app.set_status(format!("Auto-number: {error}")),
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        ContextAction::MetadataOpenAutoNumber(target) => {
            let result = app
                .pending_metadata_editor
                .as_ref()
                .ok_or_else(|| "owning metadata editor is no longer available".to_string())
                .and_then(|state| {
                    super::metadata_autonumber::AutoNumberOverlayState::new(state, target)
                });
            match result {
                Ok(overlay) => {
                    app.active_overlay = super::app::ActiveOverlay::MetadataAutoNumber(Box::new(
                        overlay,
                    ));
                }
                Err(error) => {
                    app.set_status(format!("Auto-number: {error}"));
                    if let Some(state) = app.pending_metadata_editor.take() {
                        app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                    }
                }
            }
        }
        ContextAction::MetadataAutoPopulate(target) => {
            let Some(mut state) = app.pending_metadata_editor.take() else {
                app.set_status("Auto populate: owning metadata editor is no longer available");
                return;
            };
            match super::metadata_autonumber::auto_populate(&mut state, target) {
                Ok(report) => app.set_status(report.status("Auto-populated")),
                Err(error) => app.set_status(format!("Auto populate: {error}")),
            }
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
        }
        ContextAction::MetadataCueView => {
            // Open a read-only CuePreview seeded with the row's full CUE
            // text. The display value is only a summary.
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
                let content = entry
                    .per_file_values
                    .first()
                    .cloned()
                    .unwrap_or_else(|| entry.value.clone());
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
        ContextAction::MetadataCueEdit => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let status = super::keybindings::metadata_editor_edit_embedded_cuesheet_with_system_editor(&mut state);
                app.force_redraw = true;
                app.set_status(status);
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataCueDelete => {
            if let Some(state) = app.pending_metadata_editor.take() {
                super::keybindings::open_embedded_cuesheet_delete_confirmation(app, state);
            }
        }
        ContextAction::MetadataDetailToggleRevert => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let idx = state.detail_field_idx;
                if let Some(entry) = state.active_surface_mut().entries.get_mut(idx) {
                    let mutation_report = super::probe::toggle_mb_revert_field(entry);
                    let mut status = match super::probe::mb_pill_state_field(entry) {
                        super::probe::MbRevertPill::Revert => format!(
                            "MusicBrainz values applied to {}",
                            entry.display_key
                        ),
                        super::probe::MbRevertPill::UseMb => {
                            format!("{} reverted to file values", entry.display_key)
                        }
                        super::probe::MbRevertPill::None => {
                            format!("{} unchanged", entry.display_key)
                        }
                    };
                    mutation_report.append_collapse_warning(&mut status);
                    state.recompute_active_dirty();
                    app.set_status(status);
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataDetailRestore => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let idx = state.detail_field_idx;
                if let Some(entry) = state.active_surface_mut().entries.get_mut(idx) {
                    let mutation_report = super::probe::restore_mb_proposed(entry);
                    let mut status = format!(
                        "{} restored to MusicBrainz values",
                        entry.display_key
                    );
                    mutation_report.append_collapse_warning(&mut status);
                    state.recompute_active_dirty();
                    app.set_status(status);
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
            if let Some(state) = app.pending_mb_select.take() {
                super::event_loop::accept_mb_select_release(app, tx, state);
            }
        }
        ContextAction::MbSelectCancelPicker => {
            // Cancel only the operation that owns the parked picker. A stale
            // context-menu action cannot release a newer workflow's latch.
            if let Some(state) = app.pending_mb_select.take() {
                let operation_id = state.operation_id;
                if super::event_loop::cancel_mb_select_operation(app, operation_id) {
                    app.set_status("MusicBrainz picker cancelled".to_string());
                } else {
                    app.active_overlay = super::app::ActiveOverlay::MbSelect(state);
                }
            }
        }
        ContextAction::CuePreviewSave => {
            // Reuse Command::Write — its handler consumes
            // pending_cue_preview (set by the right-click handler).
            super::command::execute_command(app, super::command::Command::Write, tx);
        }
        ContextAction::CuePreviewCancel => {
            super::command::cancel_pending_cue_preview(app);
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
            let selection = super::command::collect_selection_for_file_ops_scoped(app);
            let dropped_stale_count = selection.dropped_stale_count;
            let audio_paths: Vec<std::path::PathBuf> = selection
                .paths
                .into_iter()
                .filter(|p| {
                    app.browse
                        .entries
                        .iter()
                        .any(|e| e.path == *p && matches!(e.kind, EntryKind::AudioFile(_)))
                })
                .collect();
            super::keybindings::open_bulk_rename(app, audio_paths);
            super::command::surface_stale_selection_notice(app, dropped_stale_count);
        }
        ContextAction::CopyTo => {
            if app.browse.is_in_archive() {
                archive_synthetic_file_op_status(app, "copy/move");
                return;
            }
            let selection = super::command::collect_selection_for_file_ops_scoped(app);
            let sources = selection.paths;
            let dropped_stale_count = selection.dropped_stale_count;
            super::command::open_file_picker_for_copy_move(
                app,
                sources,
                false,
                false,
                dropped_stale_count,
            );
        }
        ContextAction::MoveTo => {
            if app.browse.is_in_archive() {
                archive_synthetic_file_op_status(app, "copy/move");
                return;
            }
            let selection = super::command::collect_selection_for_file_ops_scoped(app);
            let sources = selection.paths;
            let dropped_stale_count = selection.dropped_stale_count;
            super::command::open_file_picker_for_copy_move(
                app,
                sources,
                false,
                true,
                dropped_stale_count,
            );
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
            app.bookmarks = super::bookmarks::BookmarksState::load_from_db(&app.db);
            app.bookmarks.open_overlay();
            super::keybindings::request_bookmark_target_statuses(app, tx);
            super::keybindings::request_selected_bookmark_detail(app, tx);
        }
        ContextAction::BookmarkCurrentDir => {
            app.bookmarks = super::bookmarks::BookmarksState::load_from_db(&app.db);
            let path = app.browse.current_dir.clone();
            let name = super::bookmarks::BookmarksState::default_name_for_path(&path);
            if app.bookmarks.add_with_db(name.clone(), path, &app.db) {
                if let Some(warning) = app.bookmarks.take_warning() {
                    app.set_status(format!("bookmark added with warning: {warning}"));
                } else {
                    app.set_status(format!("bookmark added: {}", name));
                }
            } else {
                let detail = app
                    .bookmarks
                    .take_warning()
                    .unwrap_or_else(|| "bookmark could not be saved".to_string());
                app.set_status(detail);
            }
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
            app.manager.invalidate_deferred_stop_requests();
            // Retry failed items by MOVING their records back into the active
            // queue (flipping statuses in place stranded Queued rows inside the
            // completed history that the processor never scans and persistence
            // resurrected next session). Release the lock before set_status.
            let result = if let Ok(mut queue) = app.manager.queue.try_write() {
                Ok(queue.retry_all_failed())
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

const OSC52_TEXT_CLIPBOARD_MAX_BYTES: usize = 64 * 1024;

/// Publish to Tonepoet's authoritative in-process text clipboard and then make
/// one best-effort OSC 52 attempt for terminals that support it.
pub(crate) fn publish_text_clipboard(text: &str) {
    tui_file_picker::write_shared_text_clipboard(text.to_string());
    let _ = write_osc52_clipboard_to(&mut std::io::stdout(), text);
}

/// Best-effort OSC 52 system-clipboard publication. `Ok(false)` means the
/// payload exceeded the conservative terminal interoperability cap and was
/// intentionally not emitted.
pub(crate) fn write_osc52_clipboard_to(
    writer: &mut impl std::io::Write,
    text: &str,
) -> std::io::Result<bool> {
    if text.len() > OSC52_TEXT_CLIPBOARD_MAX_BYTES {
        return Ok(false);
    }
    let encoded = base64_encode(text.as_bytes());
    write!(writer, "\x1b]52;c;{}\x07", encoded)?;
    writer.flush()?;
    Ok(true)
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


fn launch_gnudb_for_split_cue_grouping_decision(
    operation_id: super::message::TagsMbOperationId,
    infos: Vec<super::cue_parser::SingleImageInfo>,
    decision: &super::command::SplitCueAlbumGroupingDecision,
    active_audio_path: Option<&std::path::Path>,
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
) {
    if !super::event_loop::gnudb_operation_is_current(app, operation_id) {
        return;
    }
    let groups = super::command::split_cue_decision_groups_as_infos(decision, &infos);
    let selected = if decision.groups.len() > 1 {
        super::command::split_cue_active_group(&groups, active_audio_path)
            .map(|group| group.to_vec())
            .unwrap_or_else(|| infos.clone())
    } else {
        groups.into_iter().next().unwrap_or(infos)
    };

    if selected.len() > 1 {
        launch_multi_single_image_gnudb(operation_id, selected, app, tx);
    } else if let Some(info) = selected.into_iter().next() {
        launch_single_image_gnudb(operation_id, info, app, tx);
    } else {
        super::event_loop::retire_gnudb_operation_with_editor_restore(app, operation_id);
        app.set_status("GNUDB: no usable CUE/image pairs for lookup");
    }
}

pub(super) fn handle_gnudb_split_cue_album_grouping_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    operation_id: super::message::TagsMbOperationId,
    infos: Vec<super::cue_parser::SingleImageInfo>,
    active_audio_path: Option<std::path::PathBuf>,
    result: Result<Box<super::command::SplitCueAlbumGroupingAsyncOutcome>, String>,
) {
    if !super::event_loop::gnudb_operation_is_current(app, operation_id) {
        return;
    }
    let outcome = match result {
        Ok(outcome) => *outcome,
        Err(err) => {
            super::event_loop::retire_gnudb_operation_with_editor_restore(app, operation_id);
            app.set_status(format!("GNUDB: split-CUE grouping failed: {err}"));
            return;
        }
    };
    super::command::store_split_cue_album_grouping_outcome(app, &infos, &outcome);
    let Some(lookup_operation_id) =
        super::event_loop::advance_gnudb_operation(app, operation_id)
    else {
        return;
    };
    launch_gnudb_for_split_cue_grouping_decision(
        lookup_operation_id,
        infos,
        &outcome.decision,
        active_audio_path.as_deref(),
        app,
        tx,
    );
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
        crate::convert::queue_expansion::expand_paths_to_all_audio(&paths)
            .into_iter()
            .filter(|p| {
                matches!(
                    crate::convert::classify::classify_file(p),
                    crate::convert::classify::EntryKind::AudioFile(_)
                )
            })
            .collect();
    super::probe::sort_paths_by_track(&mut audio_paths);

    // CUE-aware path before ordinary audio grouping. A split-side folder
    // expands to multiple image files, but GNUDB should still query each
    // CUE/image surface using the CUE track boundaries rather than treating the
    // images as two ordinary tracks. Explicit .cue selections are covered too,
    // even though they do not expand to audio paths.
    let cue_infos = super::command::collect_single_image_cue_infos_for_sources(
        &paths,
        &audio_paths,
    );
    let multi_file_cue_discovery = super::command::discover_multi_file_cues_for_sources(
        &paths,
        &audio_paths,
    );
    let multi_file_cue_layouts = &multi_file_cue_discovery.layouts;
    let active_audio_path = audio_paths
        .first()
        .cloned()
        .or_else(|| cue_infos.first().map(|info| info.audio_path.clone()))
        .or_else(|| {
            multi_file_cue_layouts
                .first()
                .and_then(|layout| layout.audio_paths.first().cloned())
        });
    if !multi_file_cue_discovery.errors.is_empty() {
        app.set_status(format!(
            "GNUDB: {}",
            multi_file_cue_discovery.errors.join("; ")
        ));
        return;
    }
    if cue_infos.is_empty() && multi_file_cue_layouts.is_empty() && audio_paths.is_empty() {
        app.set_status("No audio files for GNUDB lookup");
        return;
    }
    let operation_id = match super::event_loop::begin_gnudb_operation(app) {
        Ok(operation_id) => operation_id,
        Err(error) => {
            app.set_status(error);
            return;
        }
    };
    if !multi_file_cue_layouts.is_empty() {
        if multi_file_cue_layouts.len() != 1 {
            super::event_loop::retire_gnudb_operation_with_editor_restore(app, operation_id);
            app.set_status(
                "GNUDB: multiple native multi-FILE CUE albums are selected; \
                 select one CUE or one album copy"
                    .to_string(),
            );
            return;
        }
        let layout = multi_file_cue_layouts[0].clone();
        let disc_id = match super::cue_parser::probe_multi_file_cue(layout.clone())
            .and_then(|info| super::command::multi_file_cue_info_to_cd_sectors(&info))
            .and_then(|sectors| super::gnudb::compute_disc_id_from_sectors(&sectors))
        {
            Ok(disc_id) => disc_id,
            Err(err) => {
                super::event_loop::retire_gnudb_operation_with_editor_restore(app, operation_id);
                app.set_status(format!("GNUDB: {err}"));
                return;
            }
        };
        app.set_status(format!(
            "Querying gnudb.org (multi-FILE CUE, disc ID: {})...",
            disc_id.disc_id
        ));
        let paths_for_editor = layout.track_audio_paths;
        super::event_loop::spawn_gnudb_worker(tx.clone(), operation_id, async move {
            let result = super::gnudb::query_gnudb(&disc_id).await;
            super::message::AppMessage::GnudbQueryComplete {
                operation_id,
                result,
                paths: paths_for_editor,
            }
        });
        return;
    }
    if cue_infos.len() > 1 {
        if super::command::same_folder_split_cue_infos(&cue_infos) {
            if let Some(decision) =
                super::command::cached_or_title_split_cue_album_grouping_decision(app, &cue_infos)
            {
                launch_gnudb_for_split_cue_grouping_decision(
                    operation_id,
                    cue_infos,
                    &decision,
                    active_audio_path.as_deref(),
                    app,
                    tx,
                );
                return;
            }
            // The ladder consumes the infos only when it actually spawns;
            // clone so the not-spawned fallthrough below keeps its data.
            if super::command::spawn_split_cue_album_grouping_ladder_for_gnudb(
                app,
                tx,
                operation_id,
                cue_infos.clone(),
                active_audio_path,
            ) {
                return;
            }
        } else {
            launch_multi_single_image_gnudb(operation_id, cue_infos, app, tx);
            return;
        }
    }
    if let Some(info) = cue_infos.into_iter().next() {
        launch_single_image_gnudb(operation_id, info, app, tx);
        return;
    }

    let disc_groups = super::gnudb::group_by_disc(&audio_paths);

    if disc_groups.len() == 1 {
        // Single disc — original flow.
        let (_, group_paths) = &disc_groups[0];
        let durations =
            super::gnudb::collect_durations(group_paths, &app.browse);
        if durations.len() != group_paths.len() {
            super::event_loop::retire_gnudb_operation_with_editor_restore(app, operation_id);
            app.set_status("Probe all files first (some durations missing)");
            return;
        }
        let disc_id = super::gnudb::compute_disc_id(&durations);
        app.set_status(format!(
            "Querying gnudb.org (disc ID: {})...",
            disc_id.disc_id
        ));
        let paths_for_editor = group_paths.clone();
        super::event_loop::spawn_gnudb_worker(tx.clone(), operation_id, async move {
            let result = super::gnudb::query_gnudb(&disc_id).await;
            super::message::AppMessage::GnudbQueryComplete {
                operation_id,
                result,
                paths: paths_for_editor,
            }
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
            super::event_loop::retire_gnudb_operation_with_editor_restore(app, operation_id);
            app.set_status("Could not probe disc durations");
            return;
        }
        super::event_loop::spawn_gnudb_worker(tx.clone(), operation_id, async move {
            let attempted = disc_queries.len();
            let mut all_entries = Vec::new();
            let mut failures = Vec::new();
            for (label, disc_id, group_paths) in disc_queries {
                match super::gnudb::query_gnudb(&disc_id).await {
                    Ok(matches) => {
                        let Some(m) = matches.first() else {
                            continue;
                        };
                        match super::gnudb::read_gnudb(&m.category, &m.disc_id).await {
                            Ok(entry) => all_entries.push((label, entry, group_paths)),
                            Err(error) => failures.push(format!(
                                "{label}: read failed for {}/{}: {error}",
                                m.category, m.disc_id
                            )),
                        }
                    }
                    Err(error) => failures.push(format!(
                        "{label}: query failed for disc ID {}: {error}",
                        disc_id.disc_id
                    )),
                }
            }
            super::message::AppMessage::GnudbMultiDiscComplete {
                operation_id,
                entries: all_entries,
                failures,
                attempted,
            }
        });
    }
}

/// Launch a GNUDB query for a single-image CUE album.

/// Launch GNUDB queries for an ordered set of single-image CUE parts. This is
/// the split-side/split-disc analogue of `launch_single_image_gnudb`: each CUE
/// contributes its own disc ID from its own track boundaries, and successful
/// reads are merged into the existing multi-disc review surface. No synthetic
/// joined TOC is fabricated.
pub(super) fn launch_multi_single_image_gnudb(
    operation_id: super::message::TagsMbOperationId,
    infos: Vec<super::cue_parser::SingleImageInfo>,
    app: &mut AppState,
    tx: &tokio::sync::mpsc::Sender<super::message::AppMessage>,
) {
    let mut queries = Vec::new();
    for info in infos {
        let durations: Vec<f64> = info
            .track_boundaries
            .iter()
            .map(|&(_, count)| count as f64 / info.sample_rate as f64)
            .collect();
        if durations.is_empty() {
            continue;
        }
        let disc_id = super::gnudb::compute_disc_id(&durations);
        let label = info
            .cue_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "CUE".to_string());
        let paths_for_editor: Vec<std::path::PathBuf> = (0..info.sheet.tracks.len())
            .map(|_| info.audio_path.clone())
            .collect();
        queries.push((label, disc_id, paths_for_editor));
    }

    if queries.is_empty() {
        super::event_loop::retire_gnudb_operation_with_editor_restore(app, operation_id);
        app.set_status("GNUDB: no usable CUE/image pairs for lookup");
        return;
    }

    app.set_status(format!(
        "Querying gnudb.org ({} CUE parts)...",
        queries.len(),
    ));
    super::event_loop::spawn_gnudb_worker(tx.clone(), operation_id, async move {
        let attempted = queries.len();
        let mut all_entries = Vec::new();
        let mut failures = Vec::new();
        for (label, disc_id, paths_for_editor) in queries {
            match super::gnudb::query_gnudb(&disc_id).await {
                Ok(matches) => {
                    let Some(m) = matches.first() else {
                        continue;
                    };
                    match super::gnudb::read_gnudb(&m.category, &m.disc_id).await {
                        Ok(entry) => all_entries.push((label, entry, paths_for_editor)),
                        Err(error) => failures.push(format!(
                            "{label}: read failed for {}/{}: {error}",
                            m.category, m.disc_id
                        )),
                    }
                }
                Err(error) => failures.push(format!(
                    "{label}: query failed for disc ID {}: {error}",
                    disc_id.disc_id
                )),
            }
        }
        super::message::AppMessage::GnudbMultiDiscComplete {
            operation_id,
            entries: all_entries,
            failures,
            attempted,
        }
    });
}

pub(super) fn launch_single_image_gnudb(
    operation_id: super::message::TagsMbOperationId,
    info: super::cue_parser::SingleImageInfo,
    app: &mut AppState,
    tx: &tokio::sync::mpsc::Sender<super::message::AppMessage>,
) {
    let durations: Vec<f64> = info
        .track_boundaries
        .iter()
        .map(|&(_, count)| count as f64 / info.sample_rate as f64)
        .collect();
    if durations.is_empty() {
        super::event_loop::retire_gnudb_operation_with_editor_restore(app, operation_id);
        app.set_status("GNUDB: CUE has no usable track boundaries");
        return;
    }
    let disc_id = super::gnudb::compute_disc_id(&durations);
    app.set_status(format!(
        "Querying gnudb.org (single image, disc ID: {})...",
        disc_id.disc_id
    ));
    let paths_for_editor: Vec<std::path::PathBuf> = (0..info.sheet.tracks.len())
        .map(|_| info.audio_path.clone())
        .collect();
    super::event_loop::spawn_gnudb_worker(tx.clone(), operation_id, async move {
        let result = super::gnudb::query_gnudb(&disc_id).await;
        super::message::AppMessage::GnudbQueryComplete {
            operation_id,
            result,
            paths: paths_for_editor,
        }
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
    use crate::config::TonepoetConfig;
    use crate::tui::app::{
        AppScreen, FilePickerPurpose, MetadataEditorState, MetadataTechnicalDetails, SourceMode,
    };
    use crate::tui::message::AppMessage;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    fn app_with_selected_audio_directory(
        album: &std::path::Path,
        output_dir: Option<std::path::PathBuf>,
    ) -> AppState {
        let mut config = TonepoetConfig::default();
        config.conversion.default_destination = output_dir;
        // Make Last Used behave as enqueue-only in tests, so the assertion can
        // inspect queue publication without starting worker processing.
        config.ui.convert_default_action = "enqueue".to_string();

        let mut app = AppState::new_for_test(config);
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = album
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        app.browse.entries = vec![BrowseEntry::new(
            album.to_path_buf(),
            "album".to_string(),
            EntryKind::Directory,
            0,
            None,
        )];
        app.browse.selected_index = 0;
        app
    }

    async fn next_browse_convert_expansion(
        rx: &mut mpsc::Receiver<AppMessage>,
    ) -> (
        u64,
        crate::tui::command::BrowseConvertExpansionRequest,
        crate::tui::command::BrowseConvertExpansion,
    ) {
        match timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("folder expansion message should arrive")
            .expect("folder expansion channel should stay open")
        {
            AppMessage::BrowseConvertExpansionComplete {
                generation,
                request,
                expansion,
            } => (generation, request, expansion),
            other => panic!("expected BrowseConvertExpansionComplete, got {other:?}"),
        }
    }

    #[test]
    fn browse_context_menu_build_does_not_scan_for_cue_presence_on_reducer() {
        let source = include_str!("context_menu.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source prefix");
        assert!(
            !production.contains("std::fs::read_dir") && !production.contains("fs::read_dir"),
            "browse context-menu construction must not synchronously scan directories for CUE presence"
        );
        assert!(
            !production.contains("read_all_tags(")
                && !production.contains("read_all_tags_merged_with_metadata(")
                && !production.contains("recover_before_read"),
            "browse context-menu construction must not synchronously read or recover tags              (the cancellable worker variant inside tokio::spawn/spawn_blocking is the              approved copy-tags path)"
        );
        assert!(
            !production.contains("is_sacd_iso")
                && !production.contains("is_dvda_iso")
                && !production.contains("is_dvdv_iso")
                && !production.contains("is_bluray_iso"),
            "browse context-menu construction must not synchronously probe disc-image headers"
        );
    }

    #[tokio::test]
    async fn context_menu_convert_custom_audio_directory_expands_to_convert_review_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        let first = album.join("01 - One.flac");
        let second = album.join("02 - Two.flac");
        std::fs::write(&second, b"fixture").expect("second fixture");
        std::fs::write(&first, b"fixture").expect("first fixture");

        let (tx, mut rx) = mpsc::channel(8);
        let mut app = app_with_selected_audio_directory(&album, None);

        execute_context_action(&mut app, ContextAction::ConvertCustom, &tx, false);

        assert_eq!(app.current_screen, AppScreen::Browse);
        assert!(app.pending_browse_convert_expansion.is_some());
        let (generation, request, expansion) = next_browse_convert_expansion(&mut rx).await;
        crate::tui::command::handle_browse_convert_expansion_complete(
            &mut app,
            &tx,
            generation,
            request,
            expansion,
        );

        assert_eq!(app.current_screen, AppScreen::Convert);
        assert_eq!(app.convert.source.mode.all_paths(), vec![first, second]);
    }

    #[tokio::test]
    async fn context_menu_convert_last_used_audio_directory_commits_after_async_expansion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        let output = temp.path().join("out");
        std::fs::create_dir_all(&album).expect("album dir");
        std::fs::create_dir_all(&output).expect("output dir");
        let first = album.join("01 - One.flac");
        let second = album.join("02 - Two.flac");
        std::fs::write(&second, b"fixture").expect("second fixture");
        std::fs::write(&first, b"fixture").expect("first fixture");

        let (tx, mut rx) = mpsc::channel(8);
        let mut app = app_with_selected_audio_directory(&album, Some(output));

        execute_context_action(&mut app, ContextAction::ConvertLastUsed, &tx, false);

        let pending = app
            .pending_browse_convert_expansion
            .as_ref()
            .expect("regular directory convert should start async expansion");
        assert!(matches!(
            &pending.request.target,
            crate::tui::command::BrowseConvertExpansionTarget::ConvertReview {
                post_load: crate::tui::command::BrowseConvertPostLoad::Commit { start: false },
                ..
            }
        ));
        assert_eq!(app.current_screen, AppScreen::Browse);

        let (generation, request, expansion) = next_browse_convert_expansion(&mut rx).await;
        crate::tui::command::handle_browse_convert_expansion_complete(
            &mut app,
            &tx,
            generation,
            request,
            expansion,
        );

        assert_eq!(app.current_screen, AppScreen::Browse);
        assert!(matches!(app.convert.source.mode, SourceMode::Empty));
        let queued_paths: Vec<_> = app
            .manager
            .queue
            .try_read()
            .expect("queue read")
            .all_items()
            .into_iter()
            .map(|item| item.input_path.clone())
            .collect();
        assert_eq!(queued_paths, vec![first, second]);
    }


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

    fn menu_contains_action(
        entries: &[ContextMenuEntry],
        predicate: impl Fn(&ContextAction) -> bool + Copy,
    ) -> bool {
        entries.iter().any(|entry| match entry {
            ContextMenuEntry::Item(item) => predicate(&item.action),
            ContextMenuEntry::Submenu { children, .. } => menu_contains_action(children, predicate),
            ContextMenuEntry::Separator => false,
        })
    }

    #[test]
    fn select_submenu_uses_one_canonical_shape() {
        let ContextMenuEntry::Submenu { label, children } = build_select_submenu(true) else {
            panic!("entry selection must be a submenu");
        };
        assert_eq!(label, "Select");
        assert_eq!(
            menu_labels_recursive(&children),
            vec!["This item", "All", "Invert selection", "Deselect"]
        );

        let ContextMenuEntry::Submenu { label, children } = build_select_submenu(false) else {
            panic!("empty-space selection must be a submenu");
        };
        assert_eq!(label, "Select");
        assert_eq!(
            menu_labels_recursive(&children),
            vec!["All", "Invert selection", "Deselect"]
        );
    }

    #[test]
    fn tags_and_tagging_starts_with_metadata_editor_then_separator() {
        let ContextMenuEntry::Submenu { label, children } = build_tagging_submenu(false) else {
            panic!("tagging menu must be a submenu");
        };
        assert_eq!(label, "Tags & Tagging");
        assert!(matches!(
            children.first(),
            Some(ContextMenuEntry::Item(ContextMenuItem {
                label,
                action: ContextAction::EditMetadataFull,
                ..
            })) if label == "Edit metadata"
        ));
        assert!(matches!(children.get(1), Some(ContextMenuEntry::Separator)));
    }

    #[test]
    fn copy_tags_submenu_has_the_scoped_shape_and_honest_deferred_items() {
        let ContextMenuEntry::Submenu { children, .. } = build_tagging_submenu(false) else {
            panic!("tagging menu must be a submenu");
        };
        let copy = children.iter().find_map(|entry| match entry {
            ContextMenuEntry::Submenu { label, children } if label == "Copy tags" => Some(children),
            _ => None,
        }).expect("Copy tags submenu");
        assert_eq!(
            menu_labels_recursive(copy),
            vec![
                "All", "Canonical Only", "Artist", "Album", "Title", "Year", "Genre",
                "Performer", "Composer", "Album Artist", "Track Numbers", "Comment", "Custom...",
            ],
        );
        assert!(copy.iter().any(|entry| matches!(
            entry,
            ContextMenuEntry::Item(ContextMenuItem {
                label,
                action: ContextAction::CopyTagsCustomDeferred,
                enabled: true,
                ..
            }) if label == "Custom..."
        )));
        assert!(!menu_labels_recursive(&children).iter().any(|label| label == "Paste tags"));
        assert!(menu_labels_recursive(&children).iter().any(|label| label == "Transfer tags"));
    }

    #[test]
    fn winning_copy_tags_completion_publishes_serialized_field_blocks() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.browse.tag_clipboard_copy_generation = 9;
        app.browse.tag_clipboard_copy_active_generation = Some(9);
        app.browse.tag_clipboard_copy_cancel = Some(std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(false),
        ));
        let entry = crate::tui::probe::TagEntry {
            display_key: "TITLE".to_string(),
            item_key: lofty::tag::ItemKey::TrackTitle,
            value: "<multiple values>".to_string(),
            original: "<multiple values>".to_string(),
            is_binary: false,
            is_mixed: true,
            has_multiple_stored_values: false,
            row_scope: crate::tui::probe::RowScope::File,
            per_file_stored_value_counts: vec![1, 1],
            per_file_values: vec!["Behind the Lines".to_string(), "Duchess".to_string()],
            per_file_originals: vec!["Behind the Lines".to_string(), "Duchess".to_string()],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        };
        let mut published = None;
        let (tx, _rx) = mpsc::channel(1);

        handle_tag_clipboard_copy_complete_with_publisher(
            &mut app,
            &tx,
            9,
            vec![PathBuf::from("01.flac"), PathBuf::from("02.flac")],
            Ok((vec![entry], 0)),
            |text| published = Some(text.to_string()),
        );

        assert_eq!(
            published.as_deref(),
            Some("TITLE\nBehind the Lines\nDuchess"),
            "the generation-winning completion must invoke the text-clipboard publication seam"
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("Copied 1 field from 2 files (text clipboard)")
        );
    }

    #[test]
    fn osc52_tag_clipboard_write_is_exact_and_size_gated() {
        let mut emitted = Vec::new();
        assert!(write_osc52_clipboard_to(&mut emitted, "Duke").expect("OSC 52 write"));
        assert_eq!(emitted, b"\x1b]52;c;RHVrZQ==\x07");

        let mut oversized = Vec::new();
        assert!(!write_osc52_clipboard_to(
            &mut oversized,
            &"x".repeat(OSC52_TEXT_CLIPBOARD_MAX_BYTES + 1),
        )
        .expect("oversized OSC 52 skip"));
        assert!(oversized.is_empty(), "oversized payloads must not emit a partial escape");
    }

    #[test]
    fn superseded_transfer_reports_still_schedule_cache_refresh() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let report = super::super::tag_interchange::TagTransferReport {
            target_count: 1,
            written: 1,
            written_paths: vec![PathBuf::from("/tmp/changed.flac")],
            ..super::super::tag_interchange::TagTransferReport::default()
        };

        record_tag_transfer_report_side_effects(&mut app, &report);

        assert!(app.browse.tag_transfer_refresh_pending);
    }

    #[test]
    fn tag_transfer_progress_updates_only_the_latest_active_request() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.browse.tag_transfer_generation = 7;
        app.browse.tag_transfer_active_generation = Some(7);
        handle_tag_transfer_progress(
            &mut app,
            7,
            16,
            40,
            std::path::Path::new("/music/16 - Duchess.flac"),
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("Transfer tags: 16 of 40 files processed (16 - Duchess.flac)")
        );

        let current = app.status_message.as_ref().map(|(message, _)| message.clone());
        handle_tag_transfer_progress(
            &mut app,
            6,
            40,
            40,
            std::path::Path::new("/music/stale.flac"),
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            current,
            "superseded progress must not overwrite the current request's status"
        );
    }

    #[test]
    fn invalid_tonepoet_clipboard_status_wording_is_pinned() {
        assert_eq!(
            TONEPOET_CLIPBOARD_NO_TAG_BLOCKS_STATUS,
            "tonepoet's clipboard has no tag blocks; paste from the system clipboard with your terminal's paste key instead"
        );
    }

    #[test]
    fn metadata_tags_popup_exposes_exactly_eight_leaf_routes() {
        fn collect<'a>(entries: &'a [ContextMenuEntry], out: &mut Vec<(&'a str, &'a ContextAction)>) {
            for entry in entries {
                match entry {
                    ContextMenuEntry::Item(item) => out.push((item.label.as_str(), &item.action)),
                    ContextMenuEntry::Submenu { children, .. } => collect(children, out),
                    ContextMenuEntry::Separator => {}
                }
            }
        }

        let popup = build_metadata_tags_popup();
        let mut leaves = Vec::new();
        collect(&popup, &mut leaves);
        assert_eq!(leaves.len(), 8);
        assert!(leaves.iter().any(|(label, action)| {
            *label == "MusicBrainz" && matches!(action, ContextAction::MetadataTagsMusicBrainz)
        }));
        assert!(leaves.iter().any(|(label, action)| {
            *label == "gnuDB" && matches!(action, ContextAction::MetadataTagsGnudb)
        }));
        assert!(leaves.iter().any(|(label, action)| {
            *label == "Clipboard" && matches!(action, ContextAction::MetadataTagsClipboard)
        }));
        assert!(leaves.iter().any(|(label, action)| {
            *label == "File" && matches!(action, ContextAction::MetadataTagsFile)
        }));
        for direction in [TagTransferDirection::From, TagTransferDirection::To] {
            for scope in [TagTransferScope::Canonical, TagTransferScope::All] {
                assert!(leaves.iter().any(|(_, action)| matches!(
                    action,
                    ContextAction::MetadataTransferTags {
                        direction: actual_direction,
                        scope: actual_scope,
                    } if *actual_direction == direction && *actual_scope == scope
                )));
            }
        }
    }

    fn parked_test_editor(paths: Vec<std::path::PathBuf>) -> Box<MetadataEditorState> {
        let labels = paths
            .iter()
            .map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "track.flac".to_string())
            })
            .collect();
        Box::new(MetadataEditorState::for_files(
            paths,
            Vec::new(),
            labels,
            MetadataTechnicalDetails::default(),
        ))
    }

    #[test]
    fn metadata_tags_popup_all_eight_leaf_actions_execute_their_dispatch_routes() {
        tui_file_picker::with_scoped_shared_text_clipboard("TITLE\nDuke", || {
            let tx = {
                let (tx, _rx) = mpsc::channel(16);
                tx
            };

            let mut musicbrainz = AppState::new_for_test(TonepoetConfig::default());
            musicbrainz.pending_metadata_editor = Some(parked_test_editor(Vec::new()));
            execute_context_action(
                &mut musicbrainz,
                ContextAction::MetadataTagsMusicBrainz,
                &tx,
                false,
            );
            assert!(matches!(
                &musicbrainz.active_overlay,
                ActiveOverlay::MetadataEditor(_)
            ));
            assert_eq!(
                musicbrainz
                    .status_message
                    .as_ref()
                    .map(|(message, _)| message.as_str()),
                Some(":tags-mb: editor has no paths")
            );

            let mut gnudb = AppState::new_for_test(TonepoetConfig::default());
            gnudb.pending_metadata_editor = Some(parked_test_editor(Vec::new()));
            execute_context_action(
                &mut gnudb,
                ContextAction::MetadataTagsGnudb,
                &tx,
                false,
            );
            assert!(gnudb.pending_metadata_editor.is_some());
            assert_eq!(
                gnudb
                    .status_message
                    .as_ref()
                    .map(|(message, _)| message.as_str()),
                Some("No audio files for GNUDB lookup")
            );

            let mut clipboard = AppState::new_for_test(TonepoetConfig::default());
            clipboard.pending_metadata_editor = Some(parked_test_editor(vec![
                std::path::PathBuf::from("/tmp/editor.flac"),
            ]));
            execute_context_action(
                &mut clipboard,
                ContextAction::MetadataTagsClipboard,
                &tx,
                false,
            );
            let ActiveOverlay::MetadataEditor(state) = &clipboard.active_overlay else {
                panic!("Clipboard dispatch must restore the editor");
            };
            assert_eq!(state.active_surface().entries[0].display_key, "TITLE");
            assert_eq!(state.active_surface().entries[0].value, "Duke");

            let mut file = AppState::new_for_test(TonepoetConfig::default());
            file.pending_metadata_editor = Some(parked_test_editor(vec![
                std::path::PathBuf::from("/tmp/editor.flac"),
            ]));
            execute_context_action(
                &mut file,
                ContextAction::MetadataTagsFile,
                &tx,
                false,
            );
            let ActiveOverlay::MetadataEditor(state) = &file.active_overlay else {
                panic!("File dispatch must restore the editor");
            };
            assert!(matches!(
                state.file_picker.as_ref().map(|picker| &picker.purpose),
                Some(FilePickerPurpose::MetadataTagBlocksFile)
            ));

            for direction in [TagTransferDirection::From, TagTransferDirection::To] {
                for scope in [TagTransferScope::Canonical, TagTransferScope::All] {
                    let mut transfer = AppState::new_for_test(TonepoetConfig::default());
                    transfer.pending_metadata_editor = Some(parked_test_editor(vec![
                        std::path::PathBuf::from("/tmp/editor.flac"),
                    ]));
                    execute_context_action(
                        &mut transfer,
                        ContextAction::MetadataTransferTags { direction, scope },
                        &tx,
                        false,
                    );
                    let ActiveOverlay::MetadataEditor(state) = &transfer.active_overlay else {
                        panic!("Transfer dispatch must restore the editor");
                    };
                    assert!(matches!(
                        state.file_picker.as_ref().map(|picker| &picker.purpose),
                        Some(FilePickerPurpose::MetadataTagTransfer {
                            direction: actual_direction,
                            scope: actual_scope,
                        }) if *actual_direction == direction && *actual_scope == scope
                    ));
                }
            }
        });
    }

    #[test]
    fn browse_transfer_menu_has_four_terminal_routes_and_no_paste_stub() {
        fn collect_actions<'a>(
            entries: &'a [ContextMenuEntry],
            out: &mut Vec<&'a ContextAction>,
        ) {
            for entry in entries {
                match entry {
                    ContextMenuEntry::Item(item) => out.push(&item.action),
                    ContextMenuEntry::Submenu { children, .. } => collect_actions(children, out),
                    ContextMenuEntry::Separator => {}
                }
            }
        }

        let ContextMenuEntry::Submenu { children, .. } = build_tagging_submenu(false) else {
            panic!("tagging menu must be a submenu");
        };
        let mut actions = Vec::new();
        collect_actions(&children, &mut actions);
        let transfer_count = actions
            .iter()
            .filter(|action| matches!(action, ContextAction::BrowseTransferTags { .. }))
            .count();
        assert_eq!(transfer_count, 4);
        assert!(menu_labels_recursive(&children).iter().all(|label| label != "Paste tags"));
    }

    #[test]
    fn tag_copy_field_mapping_uses_display_keys_including_number_totals() {
        fn tag(display_key: &str) -> crate::tui::probe::TagEntry {
            crate::tui::probe::TagEntry {
                row_scope: crate::tui::probe::RowScope::File,
                display_key: display_key.to_string(),
                item_key: lofty::tag::ItemKey::Unknown("deliberately-unrelated".to_string()),
                value: "value".to_string(),
                original: "value".to_string(),
                is_binary: false,
                is_mixed: false,
                has_multiple_stored_values: false,
                per_file_stored_value_counts: vec![1],
                per_file_values: vec!["value".to_string()],
                per_file_originals: vec!["value".to_string()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            }
        }
        for key in ["TRACKNUMBER", "TRACKTOTAL", "DISCNUMBER", "DISCTOTAL"] {
            assert!(tag_entry_matches_copy_selection(
                &tag(key),
                TagCopySelection::TrackNumbers,
            ));
            assert!(tag_entry_matches_copy_selection(
                &tag(key),
                TagCopySelection::CanonicalOnly,
            ));
        }
        assert!(tag_entry_matches_copy_selection(
            &tag("DESCRIPTION"),
            TagCopySelection::All,
        ));
        assert!(!tag_entry_matches_copy_selection(
            &tag("DESCRIPTION"),
            TagCopySelection::Comment,
        ));
        assert!(tag_entry_matches_copy_selection(
            &tag("COMMENT"),
            TagCopySelection::Comment,
        ));
    }

    #[test]
    fn deferred_custom_tag_clipboard_action_emits_exact_honest_status_without_mutation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.browse.tag_clipboard = Some(crate::tui::browse::TagClipboard {
            source_paths: vec![std::path::PathBuf::from("existing.flac")],
            entries: Vec::new(),
        });
        let before_paths = app
            .browse
            .tag_clipboard
            .as_ref()
            .expect("tag clipboard")
            .source_paths
            .clone();
        let (tx, _rx) = mpsc::channel(1);

        execute_context_action(
            &mut app,
            ContextAction::CopyTagsCustomDeferred,
            &tx,
            false,
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("Custom tag selection arrives in a later round"),
        );
        let clipboard = app.browse.tag_clipboard.as_ref().expect("tag clipboard");
        assert_eq!(clipboard.source_paths, before_paths);
        assert!(clipboard.entries.is_empty());
    }

    #[test]
    fn context_menu_add_field_routes_through_editor_open_and_scrolls_input_visible() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let entries: Vec<crate::tui::probe::TagEntry> = (0..40)
            .map(|idx| crate::tui::probe::TagEntry {
                row_scope: crate::tui::probe::RowScope::File,
                display_key: format!("FIELD_{idx}"),
                item_key: lofty::tag::ItemKey::Unknown(format!("FIELD_{idx}")),
                value: "value".to_string(),
                original: "value".to_string(),
                is_binary: false,
                is_mixed: false,
                has_multiple_stored_values: false,
                per_file_stored_value_counts: vec![1],
                per_file_values: vec!["value".to_string()],
                per_file_originals: vec!["value".to_string()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            })
            .collect();
        app.pending_metadata_editor = Some(Box::new(
            crate::tui::app::MetadataEditorState::for_files(
                vec![std::path::PathBuf::from("track.flac")],
                entries,
                vec!["track.flac".to_string()],
                crate::tui::app::MetadataTechnicalDetails::default(),
            ),
        ));
        let (tx, _rx) = mpsc::channel(1);
        execute_context_action(&mut app, ContextAction::MetadataAddField, &tx, false);

        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("context action must restore the metadata editor");
        };
        assert_eq!(state.phase, crate::tui::app::MetadataEditorPhase::AddingKey);
        assert!(state.add_key_input.is_some());
        assert_eq!(state.cursor, state.active_surface().entries.len());
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        let visible = crate::tui::draw_overlays::metadata_editor_layout_for_area(
            ratatui::layout::Rect::new(0, 0, width, height),
        )
        .content_area
        .height
        .max(1) as usize;
        assert!(state.cursor >= state.scroll);
        assert!(state.cursor < state.scroll.saturating_add(visible));
    }

    #[test]
    fn tagging_submenu_cue_entries_dispatch_to_cuesheet_actions() {
        let ContextMenuEntry::Submenu { children, .. } = build_tagging_submenu(true) else {
            panic!("tagging menu must be a submenu");
        };

        assert!(menu_contains_action(&children, |action| matches!(action, ContextAction::BrowseCueView)));
        assert!(menu_contains_action(&children, |action| matches!(action, ContextAction::BrowseCueEdit)));
        assert!(menu_contains_action(&children, |action| matches!(action, ContextAction::BrowseCueDelete)));
        assert!(
            !children.iter().any(|entry| matches!(
                entry,
                ContextMenuEntry::Item(ContextMenuItem {
                    label,
                    action: ContextAction::EditMetadataFull,
                    ..
                }) if label.contains("CUE sheet")
            )),
            "CUE-labelled menu entries must not dispatch to generic Edit metadata"
        );
    }

    #[test]
    fn archive_browse_menu_includes_canonical_tagging_submenu() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.entries = vec![BrowseEntry::new(
            PathBuf::from("archive.zip"),
            "archive.zip".to_string(),
            EntryKind::Archive,
            0,
            None,
        )];
        app.browse.selected_index = 0;

        let menu = build_browse_entry_menu(&app);
        let tagging = menu.iter().find_map(|entry| match entry {
            ContextMenuEntry::Submenu { label, children } if label == "Tags & Tagging" => {
                Some(children)
            }
            _ => None,
        });
        let children = tagging.expect("archive must expose Tags & Tagging");
        assert!(matches!(
            children.first(),
            Some(ContextMenuEntry::Item(ContextMenuItem {
                label,
                action: ContextAction::EditMetadataFull,
                ..
            })) if label == "Edit metadata"
        ));
    }

    #[test]
    fn explicit_cue_file_browse_menu_includes_tagging_cuesheet_actions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cue = temp.path().join("side_a.cue");
        std::fs::write(&cue, b"FILE \"side_a.flac\" FLAC\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n")
            .expect("cue fixture");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = temp.path().to_path_buf();
        app.browse.entries = vec![BrowseEntry::new(
            cue.clone(),
            "side_a.cue".to_string(),
            EntryKind::OtherFile,
            0,
            None,
        )];
        app.browse.selected_index = 0;

        let menu = build_browse_entry_menu(&app);
        let labels = menu_labels_recursive(&menu);
        assert!(labels.iter().any(|label| label == "Tags & Tagging"));
        assert!(menu_contains_action(&menu, |action| matches!(action, ContextAction::BrowseCueView)));
        assert!(menu_contains_action(&menu, |action| matches!(action, ContextAction::BrowseCueEdit)));
        assert!(menu_contains_action(&menu, |action| matches!(action, ContextAction::BrowseCueDelete)));
    }

    fn fixture_tool_available(tool: &str) -> bool {
        std::process::Command::new(tool)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    #[test]
    fn merged_folder_browse_menu_includes_cue_actions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("album");
        std::fs::create_dir_all(&album).expect("album dir");
        std::fs::write(album.join("side_a.flac"), b"audio").expect("audio");
        std::fs::write(album.join("side_b.flac"), b"audio").expect("audio");
        for stem in ["side_a", "side_b"] {
            std::fs::write(
                album.join(format!("{stem}.cue")),
                format!(
                    "TITLE \"Album {stem}\"\nFILE \"{stem}.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 00:30:00\n"
                ),
            )
            .expect("cue");
        }
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = temp.path().to_path_buf();
        app.browse.entries = vec![BrowseEntry::new(
            album.clone(),
            "album".to_string(),
            EntryKind::Directory,
            0,
            None,
        )];
        app.browse.selected_index = 0;

        let menu = build_browse_entry_menu(&app);
        assert!(menu_contains_action(&menu, |action| matches!(action, ContextAction::BrowseCueView)));
        assert!(menu_contains_action(&menu, |action| matches!(action, ContextAction::BrowseCueEdit)));
        assert!(menu_contains_action(&menu, |action| matches!(action, ContextAction::BrowseCueDelete)));
    }

    #[test]
    fn embedded_cuesheet_audio_file_browse_menu_includes_cue_actions() {
        if !fixture_tool_available("ffmpeg") || !fixture_tool_available("metaflac") {
            eprintln!("skipping: ffmpeg/metaflac unavailable");
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let image = temp.path().join("embedded.flac");
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
        assert!(status.success());
        let cuesheet = temp.path().join("embedded.cue.txt");
        std::fs::write(
            &cuesheet,
            "TITLE \"Embedded Album\"\nFILE \"embedded.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .expect("write cuesheet");
        let status = std::process::Command::new("metaflac")
            .arg(format!("--set-tag-from-file=CUESHEET={}", cuesheet.display()))
            .arg(&image)
            .stdin(std::process::Stdio::null())
            .status()
            .expect("metaflac fixture");
        assert!(status.success());

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = temp.path().to_path_buf();
        app.browse.entries = vec![BrowseEntry::new(
            image.clone(),
            "embedded.flac".to_string(),
            EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac),
            0,
            None,
        )];
        app.browse.selected_index = 0;

        let menu = build_browse_entry_menu(&app);
        assert!(menu_contains_action(&menu, |action| matches!(action, ContextAction::BrowseCueView)));
        assert!(menu_contains_action(&menu, |action| matches!(action, ContextAction::BrowseCueEdit)));
        assert!(menu_contains_action(&menu, |action| matches!(action, ContextAction::BrowseCueDelete)));
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

        assert!(labels.iter().any(|label| label == "Properties"));
        assert!(labels.iter().any(|label| label == "Rename"));
        for forbidden in [
            "Bulk Rename",
            "Copy to...",
            "Move to...",
            "Delete permanently",
            "Analyze",
            "Tags & Tagging",
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
        assert!(labels.iter().any(|label| label == "Properties"));
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
                row_scope: crate::tui::probe::RowScope::File,
                display_key: "TITLE".into(),
                item_key: lofty::tag::ItemKey::TrackTitle,
                value: "x".into(),
                original: "x".into(),
                is_binary: false,
                is_mixed: false,
                has_multiple_stored_values: false,
                per_file_stored_value_counts: Vec::new(),
                per_file_values: vec!["x".into()],
                per_file_originals: vec!["x".into()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            },
            TagEntry {
                row_scope: crate::tui::probe::RowScope::File,
                display_key: "CUESHEET".into(),
                item_key: lofty::tag::ItemKey::Unknown("CUESHEET".into()),
                value: "FILE \"a.flac\" FLAC\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n".into(),
                original: "".into(),
                is_binary: true,
                is_mixed: false,
                has_multiple_stored_values: false,
                per_file_stored_value_counts: Vec::new(),
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

    fn mb_cardinality_entry(current: &str, proposed: &str) -> crate::tui::probe::TagEntry {
        crate::tui::probe::TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: "ARTIST".to_string(),
            item_key: lofty::tag::ItemKey::TrackArtist,
            value: current.to_string(),
            original: "Alpha; Beta".to_string(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: true,
            per_file_stored_value_counts: vec![2],
            per_file_values: vec![current.to_string()],
            per_file_originals: vec!["Alpha; Beta".to_string()],
            mb_proposed_value: Some(proposed.to_string()),
            mb_proposed_per_file: Some(vec![proposed.to_string()]),
        }
    }

    #[test]
    fn metadata_row_context_use_mb_preserves_cardinality_warning() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let state = crate::tui::app::MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/a.flac")],
            vec![mb_cardinality_entry("Alpha; Beta", "New Artist")],
            vec!["a".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        app.pending_metadata_editor = Some(Box::new(state));
        let (tx, _rx) = mpsc::channel(1);

        execute_context_action(&mut app, ContextAction::MetadataRevertMb, &tx, false);

        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or("");
        assert!(status.contains("MusicBrainz values applied to ARTIST"));
        assert!(status.contains("warning: 1 carrier"));
    }

    #[test]
    fn metadata_detail_context_use_mb_preserves_cardinality_warning() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut state = crate::tui::app::MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/a.flac")],
            vec![mb_cardinality_entry("Alpha; Beta", "New Artist")],
            vec!["a".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        state.phase = crate::tui::app::MetadataEditorPhase::DetailEdit;
        state.detail_field_idx = 0;
        app.pending_metadata_editor = Some(Box::new(state));
        let (tx, _rx) = mpsc::channel(1);

        execute_context_action(
            &mut app,
            ContextAction::MetadataDetailToggleRevert,
            &tx,
            false,
        );

        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or("");
        assert!(status.contains("MusicBrainz values applied to ARTIST"));
        assert!(status.contains("warning: 1 carrier"));
    }

    #[test]
    fn metadata_detail_context_restore_preserves_cardinality_warning() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut state = crate::tui::app::MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/a.flac")],
            vec![mb_cardinality_entry("Manual Artist", "New Artist")],
            vec!["a".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        state.phase = crate::tui::app::MetadataEditorPhase::DetailEdit;
        state.detail_field_idx = 0;
        app.pending_metadata_editor = Some(Box::new(state));
        let (tx, _rx) = mpsc::channel(1);

        execute_context_action(
            &mut app,
            ContextAction::MetadataDetailRestore,
            &tx,
            false,
        );

        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or("");
        assert!(status.contains("ARTIST restored to MusicBrainz values"));
        assert!(status.contains("warning: 1 carrier"));
    }

    #[test]
    fn properties_is_the_bottom_footer_after_copy_path_for_all_supported_entry_builders() {
        let temp = tempfile::tempdir().expect("tempdir");
        let fixtures = vec![
            (
                temp.path().join("audio.flac"),
                EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac),
            ),
            (temp.path().join("archive.zip"), EntryKind::Archive),
            (temp.path().join("disc.iso"), EntryKind::SacdIso),
            (temp.path().join("dvd.iso"), EntryKind::DvdAudioIso),
            (temp.path().join("sheet.cue"), EntryKind::OtherFile),
        ];

        for (path, kind) in fixtures {
            std::fs::write(&path, b"fixture").expect("fixture file");
            let mut app = AppState::new_for_test(TonepoetConfig::default());
            app.current_screen = AppScreen::Browse;
            app.browse.current_dir = temp.path().to_path_buf();
            app.browse.entries = vec![BrowseEntry::new(
                path.clone(),
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_string(),
                kind,
                0,
                None,
            )];
            let menu = build_browse_entry_menu(&app);
            assert!(matches!(
                menu.last(),
                Some(ContextMenuEntry::Item(ContextMenuItem {
                    label,
                    action: ContextAction::EditMetadataFull,
                    ..
                })) if label == "Properties"
            ));
            assert!(matches!(
                menu.get(menu.len().saturating_sub(2)),
                Some(ContextMenuEntry::Separator)
            ));
            assert!(matches!(
                menu.get(menu.len().saturating_sub(3)),
                Some(ContextMenuEntry::Item(ContextMenuItem {
                    action: ContextAction::CopyPath(_),
                    ..
                }))
            ));
        }

        let directory = temp.path().join("directory");
        std::fs::create_dir(&directory).expect("directory fixture");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = temp.path().to_path_buf();
        app.browse.entries = vec![BrowseEntry::new(
            directory,
            "directory".to_string(),
            EntryKind::Directory,
            0,
            None,
        )];
        let menu = build_browse_entry_menu(&app);
        assert!(matches!(
            menu.last(),
            Some(ContextMenuEntry::Item(ContextMenuItem {
                label,
                action: ContextAction::EditMetadataFull,
                ..
            })) if label == "Properties"
        ));
        assert!(matches!(
            menu.get(menu.len().saturating_sub(2)),
            Some(ContextMenuEntry::Separator)
        ));
        assert!(matches!(
            menu.get(menu.len().saturating_sub(3)),
            Some(ContextMenuEntry::Item(ContextMenuItem {
                action: ContextAction::CopyPath(_),
                ..
            }))
        ));
    }

    #[test]
    fn cue_preview_cancel_action_closes_preview_without_quitting() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.pending_cue_preview = Some(Box::new(crate::tui::app::CuePreviewState::new(
            "FILE \"album.flac\" WAVE\n".to_string(),
            std::path::PathBuf::from("/tmp/album.cue"),
            "editable preview".to_string(),
        )));
        app.active_overlay = crate::tui::app::ActiveOverlay::ContextMenu {
            levels: Vec::new(),
            origin: (0, 0),
            anchor_bottom: false,
        };
        let (tx, _rx) = mpsc::channel(4);

        execute_context_action(&mut app, ContextAction::CuePreviewCancel, &tx, false);

        assert!(app.pending_cue_preview.is_none());
        assert!(matches!(
            app.active_overlay,
            crate::tui::app::ActiveOverlay::None
        ));
        assert!(!app.should_quit);
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("CUE preview cancelled")
        );
    }

}

#[cfg(test)]
mod tag_copy_worker_coalescing_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use std::sync::atomic::Ordering;

    #[test]
    fn repeated_copy_requests_cancel_active_work_and_coalesce_to_latest() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.bulk_guard_frozen_paths = Some(vec![PathBuf::from("album")]);
        app.browse.tag_clipboard_copy_generation = 7;
        app.browse.tag_clipboard_copy_active_generation = Some(7);
        let active_cancel = Arc::new(AtomicBool::new(false));
        app.browse.tag_clipboard_copy_cancel = Some(active_cancel.clone());
        let (tx, _rx) = mpsc::channel(4);

        start_tag_clipboard_copy(&mut app, TagCopySelection::Artist, &tx);
        assert!(active_cancel.load(Ordering::Acquire));
        let first = app
            .browse
            .tag_clipboard_copy_pending
            .as_ref()
            .expect("newer request must be pending");
        assert_eq!(first.generation, 8);
        assert_eq!(first.selection, TagCopySelection::Artist);

        start_tag_clipboard_copy(&mut app, TagCopySelection::Album, &tx);
        let latest = app
            .browse
            .tag_clipboard_copy_pending
            .as_ref()
            .expect("latest request must replace earlier pending work");
        assert_eq!(latest.generation, 9);
        assert_eq!(latest.selection, TagCopySelection::Album);
        assert_eq!(app.browse.tag_clipboard_copy_active_generation, Some(7));
    }
}
