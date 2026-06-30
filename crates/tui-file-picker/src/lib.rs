//! Reusable terminal file picker / file-browser overlay.
//!
//! The crate owns navigation, filtering, folder-tree state, file operations,
//! rendering hit tests, and keyboard/mouse input. Host applications decide what
//! a selected path means and map [`HitRegion`]s or [`FilePickerAction`]s into
//! their own message systems.

mod filter;
mod input;
mod progress;
mod render;
mod state;
mod theme;
mod tree;

pub use filter::FilePickerFilter;
pub use progress::{
    ConflictAction, ConflictItemKind, ConflictPromptState, ConflictResolution, FileTaskKind, FileTaskPhase,
    FileTaskErrorRecord, FileTaskProgressState, FileTaskProgressUpdate, FileTaskScope,
    FileTaskUserAction, ProgressItem, ProgressTotals, ProgressUnit,
};
pub use state::{
    ConflictPolicyPreset, CrossDeviceCutPolicy, DeleteMode, DeletePolicy, FileOperationPolicy, FilePickerAction,
    FilePickerClipboard, FilePickerClipboardMode, FilePickerConfig, FilePickerCreateKind,
    FilePickerEntry, FilePickerError, FilePickerFocus, FilePickerHitAction,
    FilePickerMenuAction, FilePickerSelectionMode, FilePickerSortKey, FilePickerState,
    HitRegion, SaveModeConfig, SaveModeStyle, SymlinkCopyPolicy, SymlinkPolicy, ToolbarAction, TreeNode,
};
pub use theme::FilePickerTheme;

pub use ratatui::layout::Rect;
