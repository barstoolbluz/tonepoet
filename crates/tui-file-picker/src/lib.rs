//! Reusable terminal file picker / file-browser overlay.
//!
//! The crate owns navigation, filtering, folder-tree state, file operations,
//! rendering hit tests, and keyboard/mouse input. Host applications decide what
//! a selected path means and map [`HitRegion`]s or [`FilePickerAction`]s into
//! their own message systems.

pub mod click_timing;
pub mod display_width;
pub mod name_validation;
pub mod scrollbar;
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

pub use click_timing::{classify_click, ClickDisposition, ClickTracker, DOUBLE_CLICK_WINDOW};
pub use filter::FilePickerFilter;
pub use filesystem_clipboard::{remap_path_after_cut, FilesystemClipboard};
pub use text_input::{
    apply_path_completion, apply_tab_completion, apply_template_variable_completion,
    handle_text_input_key, handle_text_input_key_with_boundaries, mirror_host_clipboard_text,
    read_shared_text_clipboard,
    set_shared_clipboard_publish_hook, with_scoped_shared_text_clipboard,
    with_scoped_shared_text_clipboard_publish_hook, write_shared_text_clipboard,
    CompletionMode, TextBoundaryMode, TextInputState,
};
pub use type_ahead::{
    first_type_ahead_match, TypeAheadCandidate, TypeAheadState, TYPE_AHEAD_TIMEOUT,
};
pub use bookmarks::{
    bookmark_storage_path, initialize_bookmarks_if_absent, load_bookmarks,
    replace_bookmark_config_home_override_for_tests,
    mutate_bookmarks_atomic, mutate_bookmarks_atomic_with_reconcile,
    reconcile_bookmarks_locked, save_bookmarks_atomic, BookmarkCommit,
    BookmarkInitialization, BookmarkMoveDirection, BookmarkMutation,
    BookmarkReconciledCommit, BookmarkRecord, BookmarkSaveStatus,
};
pub use name_validation::{validate_file_name, NameValidationError};
pub use state::{rename_no_replace, RenameNoReplaceMode};
pub use source_guard::{
    capture_manifest, capture_manifest_with_cancel, capture_manifest_with_mode,
    capture_manifest_with_mode_and_cancel, digest_open_file,
    filesystem_capabilities, filesystem_identity_policy, filesystem_identity_policy_notice,
    preserve_open_file_metadata, record_filesystem_capability, rename_path_no_replace, snapshot_open_file,
    snapshot_open_handle, snapshot_path, verify_path, verify_path_with_capabilities,
    CapabilitySupport, ContentDigest, DestinationManifest, FileOperationIoCounters,
    FilesystemCapabilities, FilesystemCapabilityKind, FilesystemIdentityPolicy,
    FilesystemSemantics, RenameSourceProof, RenameVerification, Sha256, SourceEntryProof,
    SourceIdentity, SourceKind, SourceManifest, SourceSnapshot, VerifiedRemoval,
    PreparedVerifiedRemoval, InterruptedRemovalRecovery, prepare_verified_removal,
    recover_interrupted_verified_removals, recover_interrupted_verified_removals_once,
    verify_committed_rename, verify_renamed_destination,
};
pub use search::FileSearchResult;
pub use progress::{
    ConflictAction, ConflictItemKind, ConflictPromptState, ConflictResolution, FileTaskKind, FileTaskPhase,
    FileTaskCompletionReport, FileTaskErrorRecord, FileTaskProgressState, FileTaskProgressUpdate,
    FileTaskRootDisposition, FileTaskRootProof, FileTaskRootResult, FileTaskScope,
    FileTaskUndoDisposition, FileTaskUserAction, ProgressItem,
    ProgressTotals, ProgressUnit,
};
pub use state::{
    ConflictPolicyPreset, CrossDeviceCutPolicy, DeleteMode, DeletePolicy, FileOperationPolicy, FilePickerAction,
    FilePickerClipboard, FilePickerClipboardMode, FilePickerConfig, FilePickerCreateKind,
    FilePickerEntry, FilePickerError, FilePickerFocus, FilePickerHitAction,
    FilePickerMenuAction, FilePickerSelectionMode, FilePickerSortKey, FilePickerState,
    duplicate_files_in_place, execute_exact_paste_plan,
    execute_exact_paste_plan_with_proofs,
    execute_exact_paste_plan_with_proofs_and_expected_sources,
    paste_filesystem_clipboard,
    paste_filesystem_clipboard_with_retry, plan_filesystem_paste, HitRegion,
    ExactPasteProofFailure, ExactPasteProofSuccess, ExactPasteRootProof, PasteFailure,
    PasteMapping, PastePlan, PasteRetryPlan, PasteSuccess, PasteWarning, SaveModeConfig,
    SaveModeStyle, SymlinkCopyPolicy, SymlinkPolicy, ToolbarAction, TreeNode,
    VerificationMode,
};
pub use theme::FilePickerTheme;
pub use tree::{initial_tree_nodes_with_hidden, expand_tree_to_path, refresh_tree_children, child_directories};

pub use ratatui::layout::Rect;

pub use scrollbar::{ScrollbarMetrics, ScrollbarPress};
