//! Reusable terminal file picker / file-browser overlay.
//!
//! The crate owns navigation, filtering, folder-tree state, file operations,
//! rendering hit tests, and keyboard/mouse input. Host applications decide what
//! a selected path means and map [`HitRegion`]s or [`FilePickerAction`]s into
//! their own message systems.

pub mod click_timing;
pub mod display_width;
pub mod name_validation;
pub mod source_guard;
pub mod text_input;
pub mod type_ahead;
mod bookmarks;
mod filesystem_clipboard;
mod filter;
mod input;
mod progress;
mod render;
mod search;
mod state;
mod theme;
mod tree;

pub use filter::FilePickerFilter;
pub use filesystem_clipboard::{remap_path_after_cut, FilesystemClipboard};
pub use click_timing::{classify_click, ClickDisposition, ClickTracker, DOUBLE_CLICK_WINDOW};
pub use text_input::{
    apply_path_completion, apply_tab_completion, apply_template_variable_completion,
    handle_text_input_key, handle_text_input_key_with_boundaries, CompletionMode,
    TextBoundaryMode, TextInputState,
};
pub use type_ahead::{
    first_type_ahead_match, TypeAheadCandidate, TypeAheadState, TYPE_AHEAD_TIMEOUT,
};
pub use bookmarks::{
    bookmark_storage_path, initialize_bookmarks_if_absent, load_bookmarks,
    mutate_bookmarks_atomic, save_bookmarks_atomic, BookmarkCommit,
    BookmarkInitialization, BookmarkMutation, BookmarkRecord, BookmarkSaveStatus,
};
pub use name_validation::{validate_file_name, NameValidationError};
pub use source_guard::{
    capture_manifest, capture_manifest_with_cancel, digest_open_file,
    preserve_open_file_metadata, snapshot_open_file, snapshot_path, verify_path, ContentDigest,
    DestinationManifest, Sha256, SourceEntryProof, SourceIdentity, SourceKind, SourceManifest,
    SourceSnapshot,
};
pub use search::FileSearchResult;
pub use progress::{
    ConflictAction, ConflictItemKind, ConflictPromptState, ConflictResolution, FileTaskKind, FileTaskPhase,
    FileTaskCompletionReport, FileTaskErrorRecord, FileTaskProgressState, FileTaskProgressUpdate,
    FileTaskRootDisposition, FileTaskRootResult, FileTaskScope, FileTaskUserAction, ProgressItem,
    ProgressTotals, ProgressUnit,
};
pub use state::{
    ConflictPolicyPreset, CrossDeviceCutPolicy, DeleteMode, DeletePolicy, FileOperationPolicy, FilePickerAction,
    FilePickerClipboard, FilePickerClipboardMode, FilePickerConfig, FilePickerCreateKind,
    FilePickerEntry, FilePickerError, FilePickerFocus, FilePickerHitAction,
    FilePickerMenuAction, FilePickerSelectionMode, FilePickerSortKey, FilePickerState,
    duplicate_files_in_place, paste_filesystem_clipboard, plan_filesystem_paste, HitRegion,
    PasteFailure, PasteMapping, PastePlan, PasteSuccess, PasteWarning, SaveModeConfig,
    SaveModeStyle, SymlinkCopyPolicy, SymlinkPolicy, ToolbarAction, TreeNode,
};
pub use theme::FilePickerTheme;
pub use tree::{initial_tree_nodes_with_hidden, expand_tree_to_path, refresh_tree_children, child_directories};

pub use ratatui::layout::Rect;
