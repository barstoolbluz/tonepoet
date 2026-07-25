use crate::filter::FilePickerFilter;
use crate::bookmarks::{BookmarkNameAction, FilePickerBookmarks};
use crate::filesystem_clipboard::FilesystemClipboard;
use crate::search::FileSearchState;
use crate::type_ahead::TypeAheadState;
use crate::theme::FilePickerTheme;
use crate::text_input::TextInputState;
use crate::tree::{filesystem_root, initial_tree_nodes, refresh_tree_children};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// Selection policy for callers that need files, directories, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerSelectionMode {
    Files,
    Directories,
    FilesOrDirectories,
}

impl FilePickerSelectionMode {
    pub(crate) fn accepts_entry(self, is_dir: bool) -> bool {
        match self {
            Self::Files => !is_dir,
            Self::Directories => is_dir,
            Self::FilesOrDirectories => true,
        }
    }
}

impl Default for FilePickerSelectionMode {
    fn default() -> Self {
        Self::Files
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerFocus {
    Tree,
    Files,
    Address,
    Search,
    Bookmarks,
    BookmarkName,
    Menu,
    Submenu,
    Properties,
    DeleteConfirm,
    CreateName,
    SaveName,
    SaveOverwriteConfirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerSortKey {
    Name,
    Type,
    Size,
    Modified,
}

impl Default for FilePickerSortKey {
    fn default() -> Self {
        Self::Name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerClipboardMode {
    Cut,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePickerClipboard {
    pub mode: FilePickerClipboardMode,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub expanded: bool,
    pub has_children: bool,
}

/// Shared filesystem-tree construction for hosts embedding the file-picker

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePickerEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub file_type: String,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePickerAction {
    None,
    Selected(PathBuf),
    OpenSystemDefault(PathBuf),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveModeStyle {
    Inline,
    Modal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveModeConfig {
    pub default_name: String,
    pub confirm_overwrite: bool,
    pub hide_extension: Option<String>,
    pub style: SaveModeStyle,
}

/// Caller-visible policy for handling destination-name conflicts in copy/move
/// destination pickers. `Ask` preserves the interactive prompt, while
/// `Overwrite` and `Skip` let the host apply a whole-job policy before the
/// worker starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicyPreset {
    Ask,
    Overwrite,
    Skip,
}

impl Default for ConflictPolicyPreset {
    fn default() -> Self {
        Self::Ask
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    Back,
    Forward,
    Up,
    Search,
    FileOperations,
    Properties,
    Bookmarks,
    Rename,
    Duplicate,
    Delete,
    /// Accept the current picker result. In directory-selection mode this
    /// confirms the current directory rather than the highlighted child.
    AcceptSelection,
    Go,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerMenuAction {
    NewFile,
    NewFolder,
    Cut,
    Copy,
    Paste,
    Rename,
    Duplicate,
    Delete,
    SelectAll,
    InvertSelection,
    DeselectAll,
    TextCut,
    TextCopy,
    TextPaste,
    OpenSystemDefault,
    AddBookmark,
    OpenBookmarks,
}

/// The surface that owns an open file-picker context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerContextMenuKind {
    Toolbar,
    Address,
    Tree,
    File,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePickerSubmenuKind {
    New,
    Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePickerMenuEntry {
    NewSubmenu,
    SelectionSubmenu,
    Action(FilePickerMenuAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerHitAction {
    Toolbar(ToolbarAction),
    TitleToggleMaximize,
    Address,
    TreeDisclosure(usize),
    TreeRow(usize),
    FileRow(usize),
    FilesBackground,
    CreateNameEditor,
    SearchInput,
    SearchRow(usize),
    SearchClose,
    BookmarkRow(usize),
    BookmarkAdd,
    BookmarkRename,
    BookmarkDelete,
    BookmarkClose,
    ConflictPolicy(ConflictPolicyPreset),
    Menu(FilePickerMenuAction),
    MenuNew,
    MenuSelection,
    Submenu(FilePickerMenuAction),
    PropertiesClose,
    DeleteConfirm,
    DeleteCancel,
    SaveName,
    SaveCancel,
    SaveOverwriteConfirm,
    SaveOverwriteCancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitRegion {
    pub rect: Rect,
    pub action: FilePickerHitAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolbarButtonGeometry {
    pub action: ToolbarAction,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteConfirmButton {
    Delete,
    Cancel,
}

impl DeleteConfirmButton {
    pub(crate) fn toggle(self) -> Self {
        match self {
            Self::Delete => Self::Cancel,
            Self::Cancel => Self::Delete,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerCreateKind {
    File,
    Folder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePickerNameAction {
    Create(FilePickerCreateKind),
    Rename,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkCopyPolicy {
    /// Refuse to copy symlinks. This is the default because it avoids accidental
    /// cycles and preserves predictable file-manager semantics with std only.
    Reject,
    /// Copy the link target as a normal file or directory after cycle checks.
    FollowTarget,
}

impl Default for SymlinkCopyPolicy {
    fn default() -> Self {
        Self::Reject
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossDeviceCutPolicy {
    /// Refuse cross-device cut/paste if `rename` cannot complete atomically.
    Reject,
    /// Copy to the destination with staging, then delete the source under the
    /// configured delete policy.
    CopyThenDelete,
}

impl Default for CrossDeviceCutPolicy {
    fn default() -> Self {
        Self::Reject
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletePolicy {
    /// Permanently delete files and empty directories only.
    FilesAndEmptyDirectories,
    /// Permanently delete files and directories recursively. Hosts should use
    /// this only after they have opted into that destructive behavior.
    Recursive,
}

impl Default for DeletePolicy {
    fn default() -> Self {
        Self::FilesAndEmptyDirectories
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileOperationPolicy {
    /// Allow the picker to create files after the user supplies a name.
    pub allow_new_file: bool,
    /// Allow the picker to create folders after the user supplies a name.
    pub allow_new_folder: bool,
    /// Allow cut operations to place the current item on the clipboard.
    pub allow_cut: bool,
    /// Allow copy operations to place the current item on the clipboard.
    pub allow_copy: bool,
    /// Allow paste operations from the picker clipboard.
    pub allow_paste: bool,
    /// Allow delete requests. Deletion still requires confirmation.
    pub allow_delete: bool,
    /// Policy for symlinks encountered during copy.
    pub symlink_copy: SymlinkCopyPolicy,
    /// Policy for cut/paste when `rename` crosses devices.
    pub cross_device_cut: CrossDeviceCutPolicy,
    /// Policy for permanent deletion. Recursive deletion is opt-in.
    pub delete: DeletePolicy,
}

impl Default for FileOperationPolicy {
    fn default() -> Self {
        Self {
            allow_new_file: true,
            allow_new_folder: true,
            allow_cut: true,
            allow_copy: true,
            allow_paste: true,
            allow_delete: true,
            symlink_copy: SymlinkCopyPolicy::Reject,
            cross_device_cut: CrossDeviceCutPolicy::Reject,
            delete: DeletePolicy::FilesAndEmptyDirectories,
        }
    }
}

/// Compatibility alias for hosts that use file-manager terminology.
pub type DeleteMode = DeletePolicy;

/// Compatibility alias for hosts that use file-manager terminology.
pub type SymlinkPolicy = SymlinkCopyPolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePickerError {
    Io { op: &'static str, path: PathBuf, message: String },
    NotADirectory(PathBuf),
    PathNotFoundOrFiltered(PathBuf),
    EmptyAddress,
    NoSelection,
    OperationDisabled(&'static str),
    WrongSelectionMode(&'static str),
    ClipboardEmpty,
    ClipboardPathHasNoFileName(PathBuf),
    ClipboardSourceMissing(PathBuf),
    CrossDeviceMoveRejected { source: PathBuf, destination: PathBuf },
    SymlinkRejected(PathBuf),
    CopyCycleRejected { source: PathBuf, destination: PathBuf },
    InvalidNewItemName(String),
    DestinationExists(PathBuf),
    OperationCancelled,
    OperationSkipped,
    OperationCommittedWithWarning {
        source: PathBuf,
        destination: PathBuf,
        message: String,
    },
    NoPendingDelete,
    StaleHitRegions { expected: Rect, received: Rect },
}

impl FilePickerError {
    pub fn message(&self) -> String {
        match self {
            Self::Io { op, path, message } => format!("{op} failed for {}: {message}", path.display()),
            Self::NotADirectory(path) => format!("Not a directory: {}", path.display()),
            Self::PathNotFoundOrFiltered(path) => format!("Path not found or filtered out: {}", path.display()),
            Self::EmptyAddress => "Enter a path".to_string(),
            Self::NoSelection => "No item selected".to_string(),
            Self::OperationDisabled(operation) => format!("Operation disabled by file picker policy: {operation}"),
            Self::WrongSelectionMode(message) => (*message).to_string(),
            Self::ClipboardEmpty => "Nothing to paste".to_string(),
            Self::ClipboardPathHasNoFileName(path) => format!("Clipboard path has no file name: {}", path.display()),
            Self::ClipboardSourceMissing(path) => format!("Clipboard source no longer exists: {}", path.display()),
            Self::CrossDeviceMoveRejected { source, destination } => format!(
                "Cannot move {} to {} across devices under the current policy",
                source.display(),
                destination.display()
            ),
            Self::SymlinkRejected(path) => format!("Refusing to copy symlink under current policy: {}", path.display()),
            Self::CopyCycleRejected { source, destination } => format!(
                "Refusing recursive copy cycle from {} into {}",
                source.display(),
                destination.display()
            ),
            Self::InvalidNewItemName(name) => format!("Invalid name: {name}"),
            Self::DestinationExists(path) => format!("Destination already exists: {}", path.display()),
            Self::OperationCancelled => "File operation cancelled".to_string(),
            Self::OperationSkipped => "Current file operation skipped".to_string(),
            Self::OperationCommittedWithWarning { source, destination, message } => format!(
                "Operation committed from {} to {}, but requires attention: {}",
                source.display(),
                destination.display(),
                message
            ),
            Self::NoPendingDelete => "No delete is pending".to_string(),
            Self::StaleHitRegions { expected, received } => format!(
                "File picker hit regions are stale; last render area was {:?}, input area was {:?}",
                expected, received
            ),
        }
    }

    pub(crate) fn status_message(&self) -> String {
        match self {
            Self::Io { op, message, .. } if op.starts_with("delete") => {
                format!("Delete failed: {message}")
            }
            Self::Io { op, message, .. } => {
                format!("{} failed: {message}", sentence_case_operation(op))
            }
            other => other.message(),
        }
    }
}

fn sentence_case_operation(op: &str) -> String {
    let mut chars = op.chars();
    let Some(first) = chars.next() else {
        return "Operation".to_string();
    };
    let mut out = first.to_uppercase().collect::<String>();
    out.push_str(chars.as_str());
    out
}

impl fmt::Display for FilePickerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for FilePickerError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePickerConfig {
    pub start_dir: PathBuf,
    pub filter: FilePickerFilter,
    pub title: String,
    pub theme: FilePickerTheme,
    pub selection_mode: FilePickerSelectionMode,
    pub show_hidden: bool,
    /// Show a right-side preview pane when the optional image-preview feature is enabled.
    /// Image-filter pickers enable this automatically; callers may set it for custom image filters.
    pub show_preview: bool,
    /// Optional copy/move conflict-policy preset row. Most pickers leave this
    /// hidden; destination pickers set it to `Some(Ask)` or a caller-selected
    /// default.
    pub conflict_policy: Option<ConflictPolicyPreset>,
    pub operation_policy: FileOperationPolicy,
    /// Strip this extension from displayed file names and append it in save mode.
    pub hide_extension: Option<String>,
    /// Optional reusable save-as mode.
    pub save_mode: Option<SaveModeConfig>,
}

impl Default for FilePickerConfig {
    fn default() -> Self {
        Self {
            start_dir: home_dir().unwrap_or_else(|| PathBuf::from(".")),
            filter: FilePickerFilter::All,
            title: "Select file".to_string(),
            theme: FilePickerTheme::default(),
            selection_mode: FilePickerSelectionMode::Files,
            show_hidden: false,
            show_preview: false,
            conflict_policy: None,
            operation_policy: FileOperationPolicy::default(),
            hide_extension: None,
            save_mode: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FilePickerLayoutMetrics {
    pub area: Rect,
    pub tree_visible_rows: usize,
    pub file_visible_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LastClick {
    pub action: FilePickerHitAction,
    pub at: Instant,
}

#[cfg(feature = "image-preview")]
pub(crate) struct ImagePreviewLoadResult {
    pub generation: usize,
    pub path: PathBuf,
    pub result: Result<image::DynamicImage, String>,
}

#[cfg(feature = "image-preview")]
pub(crate) struct ImagePreviewCache {
    pub path: Option<PathBuf>,
    /// Most recent preview content area requested by render. This is desired
    /// geometry only; it is not evidence that the cached protocol was encoded
    /// for this area.
    pub desired_preview_area: Option<Rect>,
    /// Preview area for which `protocol` was actually prepared.
    pub encoded_preview_area: Option<Rect>,
    /// Most recent host-owned terminal image picker generation requested by
    /// render. Hosts increment this when terminal resize/cell-size changes
    /// require protocol state to be rebuilt.
    pub desired_protocol_generation: usize,
    /// Host generation for which `protocol` was actually prepared.
    pub encoded_protocol_generation: usize,
    /// Kitty-only graphics retransmit generation for which `protocol` was prepared.
    /// This is intentionally separate from protocol/cell-metric generation so
    /// mouse-damage recovery does not masquerade as a terminal resize.
    pub encoded_retransmit_generation: usize,
    /// Monotonic request generation for async preview loads.
    pub generation: usize,
    /// Generation of the decoded image currently waiting for protocol encoding.
    pub decoded_generation: Option<usize>,
    pub decoded_image: Option<image::DynamicImage>,
    pub receiver: Option<Receiver<ImagePreviewLoadResult>>,
    pub protocol: Option<Box<dyn ratatui_image::protocol::StatefulProtocol>>,
    pub error: Option<String>,
}

#[cfg(feature = "image-preview")]
impl Default for ImagePreviewCache {
    fn default() -> Self {
        Self {
            path: None,
            desired_preview_area: None,
            encoded_preview_area: None,
            desired_protocol_generation: 0,
            encoded_protocol_generation: 0,
            encoded_retransmit_generation: 0,
            generation: 0,
            decoded_generation: None,
            decoded_image: None,
            receiver: None,
            protocol: None,
            error: None,
        }
    }
}

#[cfg(feature = "image-preview")]
impl fmt::Debug for ImagePreviewCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImagePreviewCache")
            .field("path", &self.path)
            .field("desired_preview_area", &self.desired_preview_area)
            .field("encoded_preview_area", &self.encoded_preview_area)
            .field("desired_protocol_generation", &self.desired_protocol_generation)
            .field("encoded_protocol_generation", &self.encoded_protocol_generation)
            .field("encoded_retransmit_generation", &self.encoded_retransmit_generation)
            .field("generation", &self.generation)
            .field("decoded_generation", &self.decoded_generation)
            .field("has_decoded_image", &self.decoded_image.is_some())
            .field("has_receiver", &self.receiver.is_some())
            .field("has_protocol", &self.protocol.is_some())
            .field("error", &self.error)
            .finish()
    }
}

#[cfg(feature = "image-preview")]
impl Clone for ImagePreviewCache {
    fn clone(&self) -> Self {
        let mut cloned = Self::default();
        cloned.path = self.path.clone();
        cloned.desired_preview_area = self.desired_preview_area;
        cloned.encoded_preview_area = self.encoded_preview_area;
        cloned.desired_protocol_generation = self.desired_protocol_generation;
        cloned.encoded_protocol_generation = self.encoded_protocol_generation;
        cloned.encoded_retransmit_generation = self.encoded_retransmit_generation;
        cloned.generation = self.generation;
        cloned.decoded_generation = self.decoded_generation;
        cloned.error = self.error.clone();
        cloned
    }
}

const PASTE_CONTROL_RUNNING: u8 = 0;
const PASTE_CONTROL_PAUSED: u8 = 1;
const PASTE_CONTROL_ABORT: u8 = 2;
const PASTE_CONTROL_SKIP: u8 = 3;

enum PickerPasteMessage {
    Progress(crate::FileTaskProgressUpdate),
    Finished(Result<PasteSuccess, PasteFailure>),
}

pub(crate) struct PickerPasteTask {
    pub(crate) progress: crate::FileTaskProgressState,
    receiver: Option<Arc<Mutex<Receiver<PickerPasteMessage>>>>,
    control: Arc<AtomicU8>,
    clipboard: FilesystemClipboard,
    target_dir: PathBuf,
}

impl fmt::Debug for PickerPasteTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PickerPasteTask")
            .field("progress", &self.progress)
            .field("has_receiver", &self.receiver.is_some())
            .field("clipboard", &self.clipboard)
            .field("target_dir", &self.target_dir)
            .finish()
    }
}

impl Clone for PickerPasteTask {
    fn clone(&self) -> Self {
        let mut progress = self.progress.clone();
        if !progress.is_terminal() {
            progress.apply_update(crate::FileTaskProgressUpdate::Aborted {
                status: "Detached clone does not own the active paste worker".to_string(),
                totals: progress.totals,
            });
        }
        Self {
            progress,
            receiver: None,
            control: Arc::new(AtomicU8::new(PASTE_CONTROL_ABORT)),
            clipboard: self.clipboard.clone(),
            target_dir: self.target_dir.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FilePickerState {
    pub(crate) current_dir: PathBuf,
    pub(crate) history_back: Vec<PathBuf>,
    pub(crate) history_forward: Vec<PathBuf>,
    pub(crate) address_editing: bool,
    pub(crate) address_input: TextInputState,
    pub(crate) tree_nodes: Vec<TreeNode>,
    pub(crate) tree_cursor: usize,
    pub(crate) tree_scroll: usize,
    pub(crate) tree_focused: bool,
    pub(crate) entries: Vec<FilePickerEntry>,
    pub(crate) file_cursor: usize,
    pub(crate) file_scroll: usize,
    pub(crate) file_table_state: TableState,
    pub(crate) filter: FilePickerFilter,
    pub(crate) menu_open: bool,
    pub(crate) menu_cursor: usize,
    pub(crate) submenu_open: bool,
    pub(crate) submenu_cursor: usize,
    pub(crate) submenu_kind: FilePickerSubmenuKind,
    pub(crate) context_menu_kind: FilePickerContextMenuKind,
    pub(crate) context_menu_target: Option<PathBuf>,
    pub(crate) context_menu_anchor: Option<(u16, u16)>,
    pub(crate) selected: Option<PathBuf>,
    pub(crate) multi_selected: Vec<PathBuf>,
    pub(crate) title: String,
    pub(crate) theme: FilePickerTheme,
    pub(crate) focus: FilePickerFocus,
    pub(crate) previous_focus: FilePickerFocus,
    pub(crate) selection_mode: FilePickerSelectionMode,
    pub(crate) show_hidden: bool,
    pub(crate) show_preview: bool,
    pub(crate) conflict_policy: Option<ConflictPolicyPreset>,
    #[cfg(feature = "image-preview")]
    pub(crate) image_preview_cache: ImagePreviewCache,
    pub(crate) sort_key: FilePickerSortKey,
    pub(crate) sort_reverse: bool,
    pub(crate) clipboard: Option<FilesystemClipboard>,
    pub(crate) paste_task: Option<PickerPasteTask>,
    pub(crate) pending_delete: Vec<PathBuf>,
    pub(crate) delete_confirm_button: DeleteConfirmButton,
    pub(crate) properties_open: bool,
    pub(crate) last_error: Option<FilePickerError>,
    pub(crate) hit_regions: Vec<HitRegion>,
    pub(crate) toolbar_button_geometry: Vec<ToolbarButtonGeometry>,
    pub(crate) last_layout: Option<FilePickerLayoutMetrics>,
    pub(crate) last_click: Option<LastClick>,
    pub(crate) tree_last_click: Option<(PathBuf, Instant)>,
    pub(crate) double_click_window: Duration,
    pub(crate) type_ahead: TypeAheadState,
    pub(crate) search: FileSearchState,
    pub(crate) bookmarks: FilePickerBookmarks,
    pub(crate) maximized: bool,
    pub(crate) free_space_bytes: Option<u64>,
    pub(crate) operation_policy: FileOperationPolicy,
    pub(crate) pending_create: Option<FilePickerCreateKind>,
    pub(crate) pending_name_action: Option<FilePickerNameAction>,
    pub(crate) pending_name_source: Option<PathBuf>,
    pub(crate) pending_name_parent: Option<PathBuf>,
    pub(crate) create_name_input: TextInputState,
    pub(crate) hide_extension: Option<String>,
    pub(crate) save_mode: Option<SaveModeConfig>,
    pub(crate) save_name_input: TextInputState,
    pub(crate) pending_save_path: Option<PathBuf>,
}

impl FilePickerState {
    pub fn new(config: FilePickerConfig) -> Self {
        let start_dir = normalize_start_dir(&config.start_dir);
        let show_preview = config.show_preview || matches!(config.filter, FilePickerFilter::Images);
        let mut state = Self {
            current_dir: start_dir.clone(),
            history_back: Vec::new(),
            history_forward: Vec::new(),
            address_editing: false,
            address_input: TextInputState::new(start_dir.display().to_string()),
            tree_nodes: initial_tree_nodes(&start_dir),
            tree_cursor: 0,
            tree_scroll: 0,
            tree_focused: false,
            entries: Vec::new(),
            file_cursor: 0,
            file_scroll: 0,
            file_table_state: TableState::default(),
            filter: config.filter,
            menu_open: false,
            menu_cursor: 0,
            submenu_open: false,
            submenu_cursor: 0,
            submenu_kind: FilePickerSubmenuKind::New,
            context_menu_kind: FilePickerContextMenuKind::Toolbar,
            context_menu_target: None,
            context_menu_anchor: None,
            selected: None,
            multi_selected: Vec::new(),
            title: config.title,
            theme: config.theme,
            focus: FilePickerFocus::Files,
            previous_focus: FilePickerFocus::Files,
            selection_mode: config.selection_mode,
            show_hidden: config.show_hidden,
            show_preview,
            conflict_policy: config.conflict_policy,
            #[cfg(feature = "image-preview")]
            image_preview_cache: ImagePreviewCache::default(),
            sort_key: FilePickerSortKey::Name,
            sort_reverse: false,
            clipboard: None,
            paste_task: None,
            pending_delete: Vec::new(),
            delete_confirm_button: DeleteConfirmButton::Cancel,
            properties_open: false,
            last_error: None,
            hit_regions: Vec::new(),
            toolbar_button_geometry: Vec::new(),
            last_layout: None,
            last_click: None,
            tree_last_click: None,
            double_click_window: crate::click_timing::DOUBLE_CLICK_WINDOW,
            type_ahead: TypeAheadState::default(),
            search: FileSearchState::default(),
            bookmarks: FilePickerBookmarks::default(),
            maximized: false,
            free_space_bytes: None,
            operation_policy: config.operation_policy,
            pending_create: None,
            pending_name_action: None,
            pending_name_source: None,
            pending_name_parent: None,
            create_name_input: TextInputState::empty(),
            hide_extension: config.hide_extension.clone().or_else(|| {
                config.save_mode.as_ref().and_then(|save_mode| save_mode.hide_extension.clone())
            }),
            save_mode: config.save_mode.clone(),
            save_name_input: TextInputState::new(
                config
                    .save_mode
                    .as_ref()
                    .map(|save_mode| {
                        strip_configured_extension(
                            &save_mode.default_name,
                            save_mode.hide_extension.as_deref(),
                        )
                    })
                    .unwrap_or_default(),
            ),
            pending_save_path: None,
        };
        state.refresh();
        state.select_tree_node_for_current_dir();
        if state.save_mode.is_some() {
            state.focus = FilePickerFocus::SaveName;
        }
        state
    }

    pub fn current_selection(&self) -> Option<&FilePickerEntry> {
        self.entries.get(self.file_cursor)
    }

    pub fn entries(&self) -> &[FilePickerEntry] {
        &self.entries
    }

    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// Paths currently marked in the files pane. Tree rows never participate.
    pub fn multi_selected_paths(&self) -> &[PathBuf] {
        &self.multi_selected
    }

    pub fn is_path_multi_selected(&self, path: &Path) -> bool {
        self.multi_selected.iter().any(|candidate| same_path(candidate, path))
    }

    pub fn toggle_current_multi_selection(&mut self) -> bool {
        let Some(path) = self.current_selection().map(|entry| entry.path.clone()) else {
            return false;
        };
        if let Some(index) = self
            .multi_selected
            .iter()
            .position(|candidate| same_path(candidate, &path))
        {
            self.multi_selected.remove(index);
        } else {
            self.multi_selected.push(path);
        }
        true
    }

    pub fn select_all_visible(&mut self) {
        self.multi_selected = self.entries.iter().map(|entry| entry.path.clone()).collect();
    }

    pub fn invert_visible_selection(&mut self) {
        let selected: HashSet<PathBuf> = self.multi_selected.drain(..).collect();
        self.multi_selected = self
            .entries
            .iter()
            .filter(|entry| !selected.contains(&entry.path))
            .map(|entry| entry.path.clone())
            .collect();
    }

    pub fn deselect_all(&mut self) {
        self.multi_selected.clear();
    }

    pub(crate) fn action_paths(&self) -> Vec<PathBuf> {
        if self.menu_open && self.context_menu_kind == FilePickerContextMenuKind::Tree {
            return self.context_menu_target.clone().into_iter().collect();
        }
        let current = self.current_selection().map(|entry| entry.path.clone());
        if let Some(current) = current.as_ref() {
            if self.is_path_multi_selected(current) && !self.multi_selected.is_empty() {
                return self.multi_selected.clone();
            }
        }
        current.into_iter().collect()
    }

    pub(crate) fn apply_file_context_target(&mut self, index: usize) {
        self.set_file_cursor(index, self.file_visible_rows());
        let Some(path) = self.current_selection().map(|entry| entry.path.clone()) else {
            return;
        };
        if !self.is_path_multi_selected(&path) {
            self.multi_selected.clear();
            self.multi_selected.push(path);
        }
    }

    pub fn is_maximized(&self) -> bool {
        self.maximized
    }

    pub fn set_maximized(&mut self, maximized: bool) {
        self.maximized = maximized;
    }

    pub fn toggle_maximized(&mut self) {
        self.maximized = !self.maximized;
    }

    pub fn search_is_active(&self) -> bool {
        self.search.active
    }

    pub fn search_results(&self) -> &[crate::FileSearchResult] {
        &self.search.results
    }

    pub(crate) fn open_search(&mut self) {
        // An active invocation is a pure refocus operation. Preserve the
        // current query, result set, cursor/scroll position, error state, and
        // in-flight worker; critically, do not overwrite the pane captured at
        // the start of the session.
        if self.search.active {
            self.focus = FilePickerFocus::Search;
            self.type_ahead.clear();
            return;
        }

        self.previous_focus = self.focus;
        self.search.open();
        self.focus = FilePickerFocus::Search;
        self.type_ahead.clear();
        // `FileSearchState::open()` starts with an empty query, so there is no
        // filesystem walk to launch until the user edits the input.
    }

    pub(crate) fn close_search(&mut self) {
        self.search.close();
        self.focus = if matches!(self.previous_focus, FilePickerFocus::Tree) {
            FilePickerFocus::Tree
        } else {
            FilePickerFocus::Files
        };
        self.tree_focused = self.focus == FilePickerFocus::Tree;
    }

    pub(crate) fn restart_search(&mut self) {
        self.search.start(
            self.current_dir.clone(),
            self.filter.clone(),
            self.show_hidden,
        );
    }

    pub(crate) fn poll_search(&mut self) {
        self.search.poll();
    }

    pub(crate) fn open_bookmarks(&mut self) {
        self.bookmarks.reload();
        self.previous_focus = self.focus;
        self.focus = FilePickerFocus::Bookmarks;
        self.type_ahead.clear();
    }

    pub(crate) fn close_bookmarks(&mut self) {
        self.bookmarks.naming = None;
        self.focus = if matches!(self.previous_focus, FilePickerFocus::Tree) {
            FilePickerFocus::Tree
        } else {
            FilePickerFocus::Files
        };
        self.tree_focused = self.focus == FilePickerFocus::Tree;
    }

    pub(crate) fn begin_add_bookmark(&mut self, path: PathBuf) {
        self.bookmarks.reload();
        let default_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Bookmark")
            .to_string();
        self.context_menu_target = Some(path);
        self.bookmarks.naming = Some(BookmarkNameAction::Add);
        self.bookmarks.name_input = TextInputState::new_selected(default_name);
        self.focus = FilePickerFocus::BookmarkName;
    }

    pub(crate) fn begin_rename_bookmark(&mut self) {
        let Some(bookmark) = self.bookmarks.entries.get(self.bookmarks.cursor) else {
            return;
        };
        self.bookmarks.naming = Some(BookmarkNameAction::Rename(self.bookmarks.cursor));
        self.bookmarks.name_input = TextInputState::new_selected(bookmark.name.clone());
        self.focus = FilePickerFocus::BookmarkName;
    }

    pub(crate) fn cancel_bookmark_name(&mut self) {
        self.bookmarks.naming = None;
        self.bookmarks.name_input = TextInputState::empty();
        self.focus = FilePickerFocus::Bookmarks;
    }

    pub(crate) fn commit_bookmark_name(&mut self) {
        let name = self.bookmarks.name_input.text.trim().to_string();
        if name.is_empty() {
            self.bookmarks.error = Some("bookmark name cannot be empty".to_string());
            return;
        }

        let mutation = match self.bookmarks.naming {
            Some(BookmarkNameAction::Add) => {
                let path = self
                    .context_menu_target
                    .clone()
                    .unwrap_or_else(|| self.current_dir.clone());
                crate::BookmarkMutation::Add(crate::BookmarkRecord { name, path })
            }
            Some(BookmarkNameAction::Rename(index)) => {
                let Some(expected) = self.bookmarks.entries.get(index).cloned() else {
                    self.bookmarks.error = Some("bookmark no longer exists".to_string());
                    return;
                };
                crate::BookmarkMutation::Rename {
                    expected_index: index,
                    expected,
                    new_name: name,
                }
            }
            None => return,
        };

        match crate::mutate_bookmarks_atomic(mutation) {
            Ok(commit) => {
                self.bookmarks.entries = commit.entries;
                self.bookmarks.cursor = commit.affected_index;
                let warning = commit.status.warning().map(str::to_string);
                self.cancel_bookmark_name();
                self.bookmarks.error = warning;
            }
            Err(err) => {
                // Keep the naming state and exact user input intact so the
                // operation can be retried after a transient persistence error.
                self.bookmarks.error = Some(err.to_string());
            }
        }
    }

    pub(crate) fn delete_current_bookmark(&mut self) {
        let index = self.bookmarks.cursor;
        let Some(expected) = self.bookmarks.entries.get(index).cloned() else {
            return;
        };

        match crate::mutate_bookmarks_atomic(crate::BookmarkMutation::Remove {
            expected_index: index,
            expected,
        }) {
            Ok(commit) => {
                self.bookmarks.entries = commit.entries;
                self.bookmarks.cursor = commit.affected_index;
                self.bookmarks.error = commit.status.warning().map(str::to_string);
            }
            Err(err) => self.bookmarks.error = Some(err.to_string()),
        }
    }

    pub(crate) fn activate_current_bookmark(&mut self) {
        let Some(bookmark) = self.bookmarks.entries.get(self.bookmarks.cursor).cloned() else {
            return;
        };
        if !bookmark.path.is_dir() {
            self.bookmarks.error = Some(format!(
                "bookmark path no longer exists: {}",
                bookmark.path.display()
            ));
            return;
        }
        self.close_bookmarks();
        self.navigate_to_dir(bookmark.path);
    }

    pub fn focus(&self) -> FilePickerFocus {
        self.focus
    }

    pub fn sort_key(&self) -> FilePickerSortKey {
        self.sort_key
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn show_preview(&self) -> bool {
        self.show_preview
    }

    /// Return the concrete theme currently used by this picker instance.
    pub fn theme(&self) -> &FilePickerTheme {
        &self.theme
    }

    /// Replace the concrete theme used by this already-open picker.
    ///
    /// Hosts that support runtime theme changes must call this for every
    /// retained picker session after re-deriving their application theme. New
    /// pickers receive the theme through [`FilePickerConfig`], but open pickers
    /// keep their own copy so in-flight UI state is not coupled to global host
    /// state.
    pub fn set_theme(&mut self, theme: FilePickerTheme) {
        self.theme = theme;
    }

    pub fn conflict_policy(&self) -> Option<ConflictPolicyPreset> {
        self.conflict_policy
    }

    pub fn set_conflict_policy(&mut self, policy: Option<ConflictPolicyPreset>) {
        self.conflict_policy = policy;
    }

    pub fn set_show_hidden(&mut self, show_hidden: bool) {
        if self.show_hidden != show_hidden {
            self.show_hidden = show_hidden;
            self.refresh();
        }
    }

    pub fn selection_mode(&self) -> FilePickerSelectionMode {
        self.selection_mode
    }

    pub fn filter(&self) -> FilePickerFilter {
        self.filter.clone()
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn tree_cursor_path(&self) -> Option<&Path> {
        self.tree_nodes.get(self.tree_cursor).map(|node| node.path.as_path())
    }

    pub fn tree_cursor_is_expanded(&self) -> bool {
        self.tree_nodes
            .get(self.tree_cursor)
            .map(|node| node.expanded)
            .unwrap_or(false)
    }

    pub fn set_focus(&mut self, focus: FilePickerFocus) {
        self.focus = focus;
        self.tree_focused = focus == FilePickerFocus::Tree;
    }

    pub fn set_selection_mode(&mut self, selection_mode: FilePickerSelectionMode) {
        self.selection_mode = selection_mode;
    }

    pub fn free_space_bytes(&self) -> Option<u64> {
        self.free_space_bytes
    }

    pub fn set_free_space_bytes(&mut self, free_space_bytes: Option<u64>) {
        self.free_space_bytes = free_space_bytes;
    }

    /// Drop any cached image preview protocol state.
    ///
    /// Hosts should call this after terminal/cell-size changes when they do not
    /// use `render_with_image_picker` protocol generations to force a reload.
    #[cfg(feature = "image-preview")]
    pub fn invalidate_image_preview_cache(&mut self) {
        self.image_preview_cache.generation = self.image_preview_cache.generation.saturating_add(1);
        self.image_preview_cache.path = None;
        self.image_preview_cache.desired_preview_area = None;
        self.image_preview_cache.encoded_preview_area = None;
        self.image_preview_cache.desired_protocol_generation = 0;
        self.image_preview_cache.encoded_protocol_generation = 0;
        self.image_preview_cache.decoded_generation = None;
        self.image_preview_cache.decoded_image = None;
        self.image_preview_cache.receiver = None;
        self.image_preview_cache.protocol = None;
        self.image_preview_cache.error = None;
    }


    /// Start an asynchronous image-preview decode for `path` if it is not
    /// already the active request. The worker performs all disk I/O and image
    /// decoding off the render path; rendering only polls the completed result
    /// and builds terminal protocol state from already decoded pixels.
    #[cfg(feature = "image-preview")]
    pub(crate) fn request_image_preview_load(&mut self, path: PathBuf) {
        if self.image_preview_cache.path.as_ref() == Some(&path)
            && (self.image_preview_cache.receiver.is_some()
                || self.image_preview_cache.decoded_image.is_some()
                || self.image_preview_cache.error.is_some())
        {
            return;
        }

        let generation = self.image_preview_cache.generation.saturating_add(1);
        self.image_preview_cache = ImagePreviewCache {
            path: Some(path.clone()),
            desired_preview_area: None,
            encoded_preview_area: None,
            desired_protocol_generation: 0,
            encoded_protocol_generation: 0,
            encoded_retransmit_generation: 0,
            generation,
            decoded_generation: None,
            decoded_image: None,
            receiver: None,
            protocol: None,
            error: None,
        };

        let (tx, rx) = mpsc::channel();
        self.image_preview_cache.receiver = Some(rx);
        thread::spawn(move || {
            let result = std::fs::read(&path)
                .map_err(|e| format!("read failed: {e}"))
                .and_then(|bytes| image::load_from_memory(&bytes).map_err(|e| format!("decode failed: {e}")));
            let _ = tx.send(ImagePreviewLoadResult { generation, path, result });
        });
    }

    /// Clear image-preview state for non-image selections or directory changes.
    #[cfg(feature = "image-preview")]
    pub(crate) fn clear_image_preview_load(&mut self) {
        self.invalidate_image_preview_cache();
    }

    /// Ensure the current file selection has a pending or completed async
    /// preview load. This is invoked from cursor/navigation changes so render
    /// does not synchronously read or decode image files.
    #[cfg(feature = "image-preview")]
    pub(crate) fn request_image_preview_for_current_selection(&mut self) {
        let Some(entry) = self.entries.get(self.file_cursor) else {
            self.clear_image_preview_load();
            return;
        };
        if entry.is_dir || !crate::filter::is_supported_preview_image_extension(&entry.path) {
            self.clear_image_preview_load();
            return;
        }
        self.request_image_preview_load(entry.path.clone());
    }

    /// Poll at most one completed async preview decode without blocking.
    #[cfg(feature = "image-preview")]
    pub(crate) fn poll_image_preview_load(&mut self) {
        let Some(rx) = self.image_preview_cache.receiver.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                if self.image_preview_cache.path.as_ref() == Some(&result.path)
                    && self.image_preview_cache.generation == result.generation
                {
                    self.image_preview_cache.protocol = None;
                    self.image_preview_cache.encoded_preview_area = None;
                    self.image_preview_cache.encoded_protocol_generation = 0;
                    self.image_preview_cache.encoded_retransmit_generation = 0;
                    match result.result {
                        Ok(image) => {
                            self.image_preview_cache.decoded_generation = Some(result.generation);
                            self.image_preview_cache.decoded_image = Some(image);
                            self.image_preview_cache.error = None;
                        }
                        Err(error) => {
                            self.image_preview_cache.decoded_generation = Some(result.generation);
                            self.image_preview_cache.decoded_image = None;
                            self.image_preview_cache.error = Some(error);
                        }
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.image_preview_cache.receiver = Some(rx);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.image_preview_cache.error = Some("Preview worker exited before completing".to_string());
            }
        }
    }


    /// Build or rebuild terminal protocol state for a decoded image preview.
    ///
    /// This compatibility wrapper rebuilds only when the decoded image, render
    /// area, or host terminal/cell-metric generation changes. Hosts that need
    /// Kitty/Ghostty mouse-damage recovery should call
    /// `prepare_image_preview_protocol_with_retransmit_generation` instead.
    #[cfg(feature = "image-preview")]
    pub fn prepare_image_preview_protocol(
        &mut self,
        picker: &mut ratatui_image::picker::Picker,
        protocol_generation: usize,
    ) -> bool {
        self.prepare_image_preview_protocol_with_retransmit_generation(
            picker,
            protocol_generation,
            0,
        )
    }

    /// Build or rebuild terminal protocol state for a decoded image preview.
    ///
    /// `protocol_generation` is for terminal resize/cell-metric changes.
    /// `retransmit_generation` is a separate, Kitty-only damage-recovery
    /// generation that lets Ghostty/Kitty hosts rate-limit full protocol
    /// re-creation/retransmission after mouse movement without treating that
    /// recovery as a resize. Non-Kitty protocols deliberately ignore it.
    ///
    /// This method is intentionally called by the host update loop after render
    /// has recorded pane geometry, not from the render path itself.
    #[cfg(feature = "image-preview")]
    pub fn prepare_image_preview_protocol_with_retransmit_generation(
        &mut self,
        picker: &mut ratatui_image::picker::Picker,
        protocol_generation: usize,
        retransmit_generation: usize,
    ) -> bool {
        let had_receiver = self.image_preview_cache.receiver.is_some();
        self.poll_image_preview_load();
        let decode_completed = had_receiver && self.image_preview_cache.receiver.is_none();

        let Some(decoded) = self.image_preview_cache.decoded_image.as_ref() else {
            return decode_completed;
        };
        let Some(preview_area) = self.image_preview_cache.desired_preview_area else {
            return decode_completed;
        };
        if preview_area.width == 0 || preview_area.height == 0 {
            return decode_completed;
        }
        let desired_protocol_generation = protocol_generation;
        let desired_retransmit_generation = if picker.protocol_type
            == ratatui_image::picker::ProtocolType::Kitty
        {
            retransmit_generation
        } else {
            0
        };
        self.image_preview_cache.desired_protocol_generation = desired_protocol_generation;
        if self.image_preview_cache.protocol.is_some()
            && self.image_preview_cache.encoded_protocol_generation == desired_protocol_generation
            && self.image_preview_cache.encoded_retransmit_generation == desired_retransmit_generation
            && self.image_preview_cache.encoded_preview_area == Some(preview_area)
        {
            return decode_completed;
        }

        self.image_preview_cache.protocol = Some(picker.new_resize_protocol(decoded.clone()));
        self.image_preview_cache.encoded_protocol_generation = desired_protocol_generation;
        self.image_preview_cache.encoded_retransmit_generation = desired_retransmit_generation;
        self.image_preview_cache.encoded_preview_area = Some(preview_area);
        self.image_preview_cache.error = None;
        true
    }

    pub fn file_operation_policy(&self) -> FileOperationPolicy {
        self.operation_policy
    }

    pub fn set_file_operation_policy(&mut self, policy: FileOperationPolicy) {
        self.operation_policy = policy;
    }

    pub fn last_error(&self) -> Option<&FilePickerError> {
        self.last_error.as_ref()
    }

    pub fn error_message(&self) -> Option<String> {
        self.last_error.as_ref().map(FilePickerError::message)
    }

    pub(crate) fn status_error_message(&self) -> Option<String> {
        self.last_error.as_ref().map(FilePickerError::status_message)
    }

    pub fn hit_regions(&self) -> &[HitRegion] {
        &self.hit_regions
    }

    pub fn last_rendered_area(&self) -> Option<Rect> {
        self.last_layout.map(|layout| layout.area)
    }

    pub(crate) fn file_visible_rows(&self) -> usize {
        self.last_layout.map(|layout| layout.file_visible_rows.max(1)).unwrap_or(8)
    }

    pub(crate) fn tree_visible_rows(&self) -> usize {
        self.last_layout.map(|layout| layout.tree_visible_rows.max(1)).unwrap_or(8)
    }

    pub(crate) fn set_last_area(&mut self, area: Rect) {
        let current = self.last_layout.unwrap_or(FilePickerLayoutMetrics {
            area,
            tree_visible_rows: 1,
            file_visible_rows: 1,
        });
        self.last_layout = Some(FilePickerLayoutMetrics { area, ..current });
    }

    pub(crate) fn set_tree_visible_rows(&mut self, rows: usize) {
        let area = self.last_layout.map(|layout| layout.area).unwrap_or_default();
        let file_visible_rows = self.file_visible_rows();
        self.last_layout = Some(FilePickerLayoutMetrics {
            area,
            tree_visible_rows: rows.max(1),
            file_visible_rows,
        });
    }

    pub(crate) fn set_file_visible_rows(&mut self, rows: usize) {
        let area = self.last_layout.map(|layout| layout.area).unwrap_or_default();
        let tree_visible_rows = self.tree_visible_rows();
        self.last_layout = Some(FilePickerLayoutMetrics {
            area,
            tree_visible_rows,
            file_visible_rows: rows.max(1),
        });
    }

    pub(crate) fn clear_hit_regions(&mut self) {
        self.hit_regions.clear();
        self.toolbar_button_geometry.clear();
    }

    pub(crate) fn record_hit_region(&mut self, rect: Rect, action: FilePickerHitAction) {
        if rect.width > 0 && rect.height > 0 {
            self.hit_regions.push(HitRegion { rect, action });
        }
    }

    pub(crate) fn record_hit_region_clipped(
        &mut self,
        rect: Rect,
        clip: Rect,
        action: FilePickerHitAction,
    ) -> Option<Rect> {
        let clipped = intersect_rect(rect, clip)?;
        self.hit_regions.push(HitRegion { rect: clipped, action });
        Some(clipped)
    }

    pub(crate) fn record_toolbar_button_geometry(&mut self, action: ToolbarAction, rect: Rect) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        if let Some(existing) = self
            .toolbar_button_geometry
            .iter_mut()
            .find(|geometry| geometry.action == action)
        {
            existing.rect = rect;
        } else {
            self.toolbar_button_geometry.push(ToolbarButtonGeometry { action, rect });
        }
    }

    pub(crate) fn toolbar_button_rect(&self, action: ToolbarAction) -> Option<Rect> {
        self.toolbar_button_geometry
            .iter()
            .find(|geometry| geometry.action == action)
            .map(|geometry| geometry.rect)
    }

    pub(crate) fn set_error(&mut self, error: FilePickerError) {
        self.last_error = Some(error);
    }

    pub(crate) fn clear_error(&mut self) {
        self.last_error = None;
    }

    pub fn refresh(&mut self) {
        self.clear_error();
        self.entries.clear();
        let read_dir = match fs::read_dir(&self.current_dir) {
            Ok(read_dir) => read_dir,
            Err(err) => {
                self.set_error(io_error("read directory", &self.current_dir, err));
                self.file_cursor = 0;
                self.file_scroll = 0;
                self.selected = None;
                #[cfg(feature = "image-preview")]
                self.clear_image_preview_load();
                self.sync_address_from_current_dir();
                return;
            }
        };

        let mut entries = Vec::new();
        for item in read_dir {
            let Ok(item) = item else {
                continue;
            };
            let path = item.path();
            let raw_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let name = strip_configured_extension(&raw_name, self.hide_extension.as_deref());
            if !self.show_hidden && name.starts_with('.') {
                continue;
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let is_symlink = metadata.file_type().is_symlink();
            let is_dir = metadata.file_type().is_dir();
            if !self.filter.accepts_path(&path, is_dir) {
                continue;
            }
            entries.push(FilePickerEntry {
                name,
                path,
                is_dir,
                size: metadata.is_file().then_some(metadata.len()),
                file_type: file_type_label(item.path(), is_dir, is_symlink),
                modified: metadata.modified().ok(),
            });
        }

        let sort_key = self.sort_key;
        let reverse = self.sort_reverse;
        entries.sort_by(|a, b| compare_entries(a, b, sort_key, reverse));
        self.entries = entries;
        let visible_paths: HashSet<PathBuf> = self.entries.iter().map(|entry| entry.path.clone()).collect();
        self.multi_selected.retain(|path| visible_paths.contains(path));
        if self.entries.is_empty() {
            self.file_cursor = 0;
            self.file_scroll = 0;
            self.selected = None;
        } else {
            self.file_cursor = self.file_cursor.min(self.entries.len() - 1);
            self.selected = self.entries.get(self.file_cursor).map(|entry| entry.path.clone());
            self.ensure_file_cursor_visible(self.file_visible_rows());
        }
        self.sync_address_from_current_dir();
        #[cfg(feature = "image-preview")]
        self.request_image_preview_for_current_selection();
        refresh_tree_children(&mut self.tree_nodes, &self.current_dir, self.show_hidden);
        self.select_tree_node_for_current_dir();
    }

    /// Adopt a directory path after a filesystem mutation has already
    /// committed. Unlike ordinary navigation, this deliberately performs no
    /// preflight `is_dir` check: retaining the old path would be incorrect and
    /// unrecoverable if the post-commit metadata probe is temporarily denied.
    fn adopt_committed_current_dir(&mut self, dir: PathBuf) {
        self.current_dir = dir;
        self.file_cursor = 0;
        self.file_scroll = 0;
        self.selected = None;
        #[cfg(feature = "image-preview")]
        self.clear_image_preview_load();
        self.refresh();
        self.select_tree_node_for_current_dir();
    }

    pub fn navigate_to_dir(&mut self, dir: PathBuf) -> bool {
        self.navigate_to_dir_with_history(dir, true)
    }

    pub(crate) fn navigate_to_dir_with_history(&mut self, dir: PathBuf, add_history: bool) -> bool {
        let dir = normalize_path_for_navigation(&self.current_dir, &dir);
        if !dir.is_dir() {
            self.set_error(FilePickerError::NotADirectory(dir));
            return false;
        }
        if same_path(&self.current_dir, &dir) {
            self.refresh();
            return true;
        }
        if add_history {
            self.history_back.push(self.current_dir.clone());
            self.history_forward.clear();
        }
        self.current_dir = dir;
        self.file_cursor = 0;
        self.file_scroll = 0;
        self.selected = None;
        #[cfg(feature = "image-preview")]
        self.clear_image_preview_load();
        self.menu_open = false;
        self.submenu_open = false;
        self.properties_open = false;
        self.pending_delete.clear();
        self.delete_confirm_button = DeleteConfirmButton::Cancel;
        self.pending_create = None;
        self.focus = FilePickerFocus::Files;
        self.previous_focus = FilePickerFocus::Files;
        self.tree_focused = false;
        self.refresh();
        true
    }

    pub fn go_back(&mut self) -> bool {
        let Some(prior) = self.history_back.last().cloned() else {
            return false;
        };
        let current = self.current_dir.clone();
        if !self.navigate_to_dir_with_history(prior, false) {
            return false;
        }
        self.history_back.pop();
        self.history_forward.push(current);
        true
    }

    pub fn go_forward(&mut self) -> bool {
        let Some(next) = self.history_forward.last().cloned() else {
            return false;
        };
        let current = self.current_dir.clone();
        if !self.navigate_to_dir_with_history(next, false) {
            return false;
        }
        self.history_forward.pop();
        self.history_back.push(current);
        true
    }

    pub fn go_parent(&mut self) -> bool {
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return false;
        };
        self.navigate_to_dir(parent)
    }

    pub fn commit_address(&mut self) -> FilePickerAction {
        let input = self.address_input.text.trim();
        if input.is_empty() {
            self.set_error(FilePickerError::EmptyAddress);
            return FilePickerAction::None;
        }
        let expanded = expand_user_path(input);
        let candidate = normalize_path_for_navigation(&self.current_dir, &expanded);
        if candidate.is_dir() {
            self.address_editing = false;
            self.focus = FilePickerFocus::Files;
            self.navigate_to_dir(candidate);
            FilePickerAction::None
        } else if candidate.is_file() && self.filter.accepts_path(&candidate, false) {
            if !self.selection_mode.accepts_entry(false) {
                self.set_error(FilePickerError::WrongSelectionMode(
                    "This picker is configured for directory selection",
                ));
                return FilePickerAction::None;
            }
            self.address_editing = false;
            self.focus = FilePickerFocus::Files;
            if let Some(parent) = candidate.parent().map(Path::to_path_buf) {
                self.navigate_to_dir(parent);
                if let Some(pos) = self.entries.iter().position(|entry| same_path(&entry.path, &candidate)) {
                    self.set_file_cursor(pos, self.file_visible_rows());
                }
            }
            FilePickerAction::Selected(candidate)
        } else {
            self.set_error(FilePickerError::PathNotFoundOrFiltered(candidate));
            FilePickerAction::None
        }
    }

    pub fn sync_address_from_current_dir(&mut self) {
        self.address_input = TextInputState::new(self.current_dir.display().to_string());
    }

    pub fn begin_address_edit(&mut self) {
        self.address_editing = true;
        self.previous_focus = self.focus;
        self.focus = FilePickerFocus::Address;
        self.menu_open = false;
        self.submenu_open = false;
        self.sync_address_from_current_dir();
        self.address_input.select_all_text();
    }

    pub fn cancel_address_edit(&mut self) {
        self.address_editing = false;
        self.focus = FilePickerFocus::Files;
        self.sync_address_from_current_dir();
    }

    pub fn set_file_cursor(&mut self, index: usize, visible_rows: usize) {
        if self.entries.is_empty() {
            self.file_cursor = 0;
            self.file_scroll = 0;
            self.selected = None;
            #[cfg(feature = "image-preview")]
            self.clear_image_preview_load();
            return;
        }
        self.file_cursor = index.min(self.entries.len() - 1);
        self.selected = self.entries.get(self.file_cursor).map(|entry| entry.path.clone());
        self.ensure_file_cursor_visible(visible_rows);
        #[cfg(feature = "image-preview")]
        self.request_image_preview_for_current_selection();
    }

    pub fn move_file_cursor(&mut self, delta: isize, visible_rows: usize) {
        if self.entries.is_empty() {
            self.file_cursor = 0;
            self.file_scroll = 0;
            self.selected = None;
            #[cfg(feature = "image-preview")]
            self.clear_image_preview_load();
            return;
        }
        let step = delta.unsigned_abs();
        let next = if delta.is_negative() {
            self.file_cursor.saturating_sub(step)
        } else {
            self.file_cursor.saturating_add(step).min(self.entries.len() - 1)
        };
        self.set_file_cursor(next, visible_rows);
    }

    pub fn ensure_file_cursor_visible(&mut self, visible_rows: usize) {
        let visible_rows = visible_rows.max(1);
        if self.file_cursor < self.file_scroll {
            self.file_scroll = self.file_cursor;
        } else if self.file_cursor >= self.file_scroll.saturating_add(visible_rows) {
            self.file_scroll = self.file_cursor.saturating_add(1).saturating_sub(visible_rows);
        }
        let max_scroll = self.entries.len().saturating_sub(visible_rows);
        self.file_scroll = self.file_scroll.min(max_scroll);
    }

    pub fn move_tree_cursor(&mut self, delta: isize, visible_rows: usize) {
        if self.tree_nodes.is_empty() {
            self.tree_cursor = 0;
            self.tree_scroll = 0;
            return;
        }
        let step = delta.unsigned_abs();
        let next = if delta.is_negative() {
            self.tree_cursor.saturating_sub(step)
        } else {
            self.tree_cursor.saturating_add(step).min(self.tree_nodes.len() - 1)
        };
        self.tree_cursor = next;
        self.ensure_tree_cursor_visible(visible_rows.max(1));
    }

    pub fn ensure_tree_cursor_visible(&mut self, visible_rows: usize) {
        let visible_rows = visible_rows.max(1);
        if self.tree_cursor < self.tree_scroll {
            self.tree_scroll = self.tree_cursor;
        } else if self.tree_cursor >= self.tree_scroll.saturating_add(visible_rows) {
            self.tree_scroll = self.tree_cursor.saturating_add(1).saturating_sub(visible_rows);
        }
        let max_scroll = self.tree_nodes.len().saturating_sub(visible_rows);
        self.tree_scroll = self.tree_scroll.min(max_scroll);
    }

    pub fn set_tree_cursor(&mut self, index: usize, visible_rows: usize) {
        if self.tree_nodes.is_empty() {
            self.tree_cursor = 0;
            self.tree_scroll = 0;
            return;
        }
        self.tree_cursor = index.min(self.tree_nodes.len() - 1);
        self.ensure_tree_cursor_visible(visible_rows);
    }

    pub fn activate_tree_cursor(&mut self) {
        let Some(node) = self.tree_nodes.get(self.tree_cursor).cloned() else {
            return;
        };
        if node.has_children && !node.expanded {
            self.toggle_tree_node(self.tree_cursor);
        }
        self.navigate_to_dir(node.path);
    }

    pub fn tree_right(&mut self) {
        let Some(node) = self.tree_nodes.get(self.tree_cursor).cloned() else {
            return;
        };
        if node.has_children && !node.expanded {
            self.toggle_tree_node(self.tree_cursor);
            self.ensure_tree_cursor_visible(self.tree_visible_rows());
            return;
        }
        if !same_path(&self.current_dir, &node.path) {
            self.navigate_to_dir(node.path);
            self.focus = FilePickerFocus::Tree;
            self.tree_focused = true;
            return;
        }
        self.focus = FilePickerFocus::Files;
        self.tree_focused = false;
    }

    pub fn tree_left(&mut self) {
        let Some(node) = self.tree_nodes.get(self.tree_cursor).cloned() else {
            return;
        };
        if node.expanded {
            self.toggle_tree_node(self.tree_cursor);
            return;
        }
        let Some(parent) = node.path.parent().map(Path::to_path_buf) else {
            return;
        };
        if let Some(index) = self.tree_nodes.iter().position(|candidate| same_path(&candidate.path, &parent)) {
            self.set_tree_cursor(index, self.tree_visible_rows());
        }
        self.navigate_to_dir(parent);
        self.focus = FilePickerFocus::Tree;
        self.tree_focused = true;
    }

    pub fn toggle_tree_node(&mut self, index: usize) {
        if index >= self.tree_nodes.len() {
            return;
        }
        if self.tree_nodes[index].expanded {
            let depth = self.tree_nodes[index].depth;
            self.tree_nodes[index].expanded = false;
            let remove_start = index + 1;
            let remove_end = self.tree_nodes[remove_start..]
                .iter()
                .position(|node| node.depth <= depth)
                .map(|pos| remove_start + pos)
                .unwrap_or(self.tree_nodes.len());
            self.tree_nodes.drain(remove_start..remove_end);
            self.tree_cursor = self.tree_cursor.min(self.tree_nodes.len().saturating_sub(1));
        } else {
            let path = self.tree_nodes[index].path.clone();
            if let Some(node) = self.tree_nodes.get_mut(index) {
                node.expanded = true;
            }
            refresh_tree_children(&mut self.tree_nodes, &path, self.show_hidden);
        }
    }

    pub fn select_tree_node_for_current_dir(&mut self) {
        if let Some(index) = self
            .tree_nodes
            .iter()
            .position(|node| same_path(&node.path, &self.current_dir))
        {
            self.tree_cursor = index;
            return;
        }
        let mut cursor_path = self.current_dir.as_path();
        while let Some(parent) = cursor_path.parent() {
            if let Some(index) = self.tree_nodes.iter().position(|node| same_path(&node.path, parent)) {
                self.tree_cursor = index;
                return;
            }
            cursor_path = parent;
        }
    }

    pub fn open_or_select_current(&mut self) -> FilePickerAction {
        let Some(entry) = self.entries.get(self.file_cursor).cloned() else {
            return FilePickerAction::None;
        };
        self.selected = Some(entry.path.clone());
        if entry.is_dir {
            self.navigate_to_dir(entry.path);
            FilePickerAction::None
        } else if self.selection_mode.accepts_entry(false) {
            FilePickerAction::Selected(entry.path)
        } else {
            self.set_error(FilePickerError::WrongSelectionMode("This picker is configured for directory selection"));
            FilePickerAction::None
        }
    }

    pub fn accept_current_selection(&mut self) -> FilePickerAction {
        if self.selection_mode == FilePickerSelectionMode::Directories {
            return FilePickerAction::Selected(self.current_dir.clone());
        }
        let Some(entry) = self.entries.get(self.file_cursor).cloned() else {
            self.set_error(FilePickerError::NoSelection);
            return FilePickerAction::None;
        };
        if self.selection_mode.accepts_entry(entry.is_dir) {
            FilePickerAction::Selected(entry.path)
        } else {
            self.set_error(FilePickerError::WrongSelectionMode("This picker cannot select that item type"));
            FilePickerAction::None
        }
    }

    pub fn set_sort(&mut self, sort_key: FilePickerSortKey) {
        if self.sort_key == sort_key {
            self.sort_reverse = !self.sort_reverse;
        } else {
            self.sort_key = sort_key;
            self.sort_reverse = false;
        }
        self.refresh();
    }

    fn capture_pane_focus_for_modal_transition(&mut self) {
        if !matches!(self.focus, FilePickerFocus::Menu | FilePickerFocus::Submenu) {
            self.previous_focus = self.focus;
        }
    }

    fn restore_pane_focus(&mut self) {
        self.focus = if self.previous_focus == FilePickerFocus::Tree {
            FilePickerFocus::Tree
        } else {
            FilePickerFocus::Files
        };
        self.tree_focused = self.focus == FilePickerFocus::Tree;
    }

    pub(crate) fn begin_create_name(&mut self, kind: FilePickerCreateKind) {
        self.begin_create_name_in(kind, self.current_dir.clone());
    }

    pub(crate) fn begin_create_name_in(&mut self, kind: FilePickerCreateKind, parent: PathBuf) {
        match kind {
            FilePickerCreateKind::File if !self.operation_policy.allow_new_file => {
                self.set_error(FilePickerError::OperationDisabled("new file"));
                return;
            }
            FilePickerCreateKind::Folder if !self.operation_policy.allow_new_folder => {
                self.set_error(FilePickerError::OperationDisabled("new folder"));
                return;
            }
            _ => {}
        }
        self.pending_create = Some(kind);
        self.pending_name_action = Some(FilePickerNameAction::Create(kind));
        self.pending_name_source = None;
        self.pending_name_parent = Some(parent.clone());
        let initial_name = match kind {
            FilePickerCreateKind::File => unique_path(&parent.join("untitled.txt"))
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("untitled.txt")
                .to_string(),
            FilePickerCreateKind::Folder => unique_path(&parent.join("New Folder"))
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("New Folder")
                .to_string(),
        };
        self.create_name_input = TextInputState::new_selected(initial_name);
        self.capture_pane_focus_for_modal_transition();
        self.menu_open = false;
        self.submenu_open = false;
        self.focus = FilePickerFocus::CreateName;
        self.clear_error();
    }

    pub(crate) fn cancel_create_name(&mut self) {
        self.pending_create = None;
        self.pending_name_action = None;
        self.pending_name_source = None;
        self.pending_name_parent = None;
        self.create_name_input = TextInputState::empty();
        self.restore_pane_focus();
        self.clear_error();
    }

    pub(crate) fn commit_create_name(&mut self) -> bool {
        let Some(action) = self.pending_name_action else {
            self.set_error(FilePickerError::InvalidNewItemName("no pending item action".to_string()));
            return false;
        };
        let name = self.create_name_input.text.trim().to_string();
        let result = match action {
            FilePickerNameAction::Create(kind) => self.try_create_named_item(kind, &name),
            FilePickerNameAction::Rename => self.try_rename_current(&name),
            FilePickerNameAction::Duplicate => self.try_duplicate_current(&name),
        };
        match result {
            Ok(()) => {
                self.pending_create = None;
                self.pending_name_action = None;
                self.pending_name_source = None;
                self.pending_name_parent = None;
                self.create_name_input = TextInputState::empty();
                self.restore_pane_focus();
                true
            }
            Err(err) => {
                self.set_error(err);
                false
            }
        }
    }


    pub(crate) fn begin_rename_current(&mut self) -> bool {
        let Some(path) = self.current_selection().map(|entry| entry.path.clone()) else {
            self.set_error(FilePickerError::NoSelection);
            return false;
        };
        self.begin_rename_path(path)
    }

    pub(crate) fn begin_rename_path(&mut self, path: PathBuf) -> bool {
        let Some(name) = path.file_name().and_then(OsStr::to_str).map(str::to_string) else {
            self.set_error(FilePickerError::InvalidNewItemName(path.display().to_string()));
            return false;
        };
        self.pending_create = None;
        self.pending_name_action = Some(FilePickerNameAction::Rename);
        self.pending_name_source = Some(path.clone());
        self.pending_name_parent = path.parent().map(Path::to_path_buf);
        self.create_name_input = TextInputState::new_selected(strip_configured_extension(
            &name,
            self.hide_extension.as_deref(),
        ));
        self.capture_pane_focus_for_modal_transition();
        self.menu_open = false;
        self.submenu_open = false;
        self.focus = FilePickerFocus::CreateName;
        self.clear_error();
        true
    }

    pub(crate) fn begin_duplicate_current(&mut self) -> bool {
        let Some(path) = self.current_selection().map(|entry| entry.path.clone()) else {
            self.set_error(FilePickerError::NoSelection);
            return false;
        };
        self.begin_duplicate_path(path)
    }

    pub(crate) fn begin_duplicate_path(&mut self, path: PathBuf) -> bool {
        if !path.is_file() {
            self.set_error(FilePickerError::WrongSelectionMode("Duplicate supports files only"));
            return false;
        }
        self.pending_create = None;
        self.pending_name_action = Some(FilePickerNameAction::Duplicate);
        self.pending_name_source = Some(path.clone());
        self.pending_name_parent = path.parent().map(Path::to_path_buf);
        let source_name = path.file_name().and_then(OsStr::to_str).unwrap_or("item");
        let stem = strip_configured_extension(source_name, self.hide_extension.as_deref());
        let candidate = append_configured_extension(format!("{}-copy", stem), self.hide_extension.as_deref());
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        self.create_name_input = TextInputState::new_selected(strip_configured_extension(
            unique_path(&parent.join(candidate))
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("copy"),
            self.hide_extension.as_deref(),
        ));
        self.capture_pane_focus_for_modal_transition();
        self.menu_open = false;
        self.submenu_open = false;
        self.focus = FilePickerFocus::CreateName;
        self.clear_error();
        true
    }

    pub(crate) fn duplicate_action_paths(&mut self) -> Result<(), FilePickerError> {
        let paths = self.action_paths();
        if paths.is_empty() {
            return Err(FilePickerError::NoSelection);
        }
        if paths.len() == 1 {
            self.begin_duplicate_path(paths[0].clone());
            return Ok(());
        }

        let destinations = duplicate_files_in_place(&paths, self.operation_policy)?;
        self.multi_selected = destinations.clone();
        self.selected = destinations.last().cloned();
        self.refresh();
        if let Some(path) = self.selected.clone() {
            self.select_path_in_entries(&path);
        }
        Ok(())
    }

    pub(crate) fn try_rename_current(&mut self, display_name: &str) -> Result<(), FilePickerError> {
        validate_new_item_name(display_name)?;
        let source = self
            .pending_name_source
            .clone()
            .or_else(|| self.selected.clone())
            .or_else(|| self.entries.get(self.file_cursor).map(|entry| entry.path.clone()))
            .ok_or(FilePickerError::NoSelection)?;
        let parent = self
            .pending_name_parent
            .clone()
            .or_else(|| source.parent().map(Path::to_path_buf))
            .ok_or(FilePickerError::NoSelection)?;
        let file_name = append_configured_extension(display_name.trim().to_string(), self.hide_extension.as_deref());
        let destination = parent.join(file_name);
        if destination == source {
            return Ok(());
        }
        let remapped_current = self
            .current_dir
            .strip_prefix(&source)
            .ok()
            .map(|suffix| destination.join(suffix));
        rename_no_replace(&source, &destination).map_err(|err| {
            if err.kind() == io::ErrorKind::AlreadyExists {
                FilePickerError::DestinationExists(destination.clone())
            } else {
                io_error("rename without replacing", &source, err)
            }
        })?;
        for path in self
            .history_back
            .iter_mut()
            .chain(self.history_forward.iter_mut())
        {
            if let Ok(suffix) = path.strip_prefix(&source) {
                *path = destination.join(suffix);
            }
        }
        refresh_tree_children(&mut self.tree_nodes, &parent, self.show_hidden);
        if let Some(current) = remapped_current {
            // The filesystem mutation is already committed. Do not route the
            // repaired path back through fallible pre-navigation validation:
            // a transient metadata/permission error must not leave current_dir
            // pointing at the vanished pre-rename path.
            self.adopt_committed_current_dir(current);
        } else if same_path(&parent, &self.current_dir) {
            self.selected = Some(destination.clone());
            self.refresh();
            self.select_path_in_entries(&destination);
        } else {
            self.select_tree_node_for_current_dir();
        }
        if let Err(err) = sync_directory(&parent) {
            // The rename is already visible and must not be presented as an
            // uncommitted failure. Keep the repaired in-memory state and surface
            // a durability warning while returning success to close the editor.
            self.set_error(committed_operation_warning(
                &source,
                &destination,
                format!("rename committed, but parent-directory synchronization failed: {err}"),
            ));
        }
        Ok(())
    }

    pub(crate) fn try_duplicate_current(&mut self, display_name: &str) -> Result<(), FilePickerError> {
        validate_new_item_name(display_name)?;
        let source = self
            .pending_name_source
            .clone()
            .or_else(|| self.entries.get(self.file_cursor).map(|entry| entry.path.clone()))
            .ok_or(FilePickerError::NoSelection)?;
        if !source.is_file() {
            return Err(FilePickerError::WrongSelectionMode("Duplicate supports files only"));
        }
        let parent = self
            .pending_name_parent
            .clone()
            .or_else(|| source.parent().map(Path::to_path_buf))
            .ok_or(FilePickerError::NoSelection)?;
        let file_name = append_configured_extension(display_name.trim().to_string(), self.hide_extension.as_deref());
        let destination = parent.join(file_name);
        if destination.exists() {
            return Err(FilePickerError::DestinationExists(destination));
        }
        safe_copy_path(&source, &destination, self.operation_policy)?;
        self.selected = Some(destination.clone());
        self.refresh();
        self.select_path_in_entries(&destination);
        Ok(())
    }

    pub(crate) fn commit_save_name(&mut self) -> FilePickerAction {
        let Some(save_mode) = self.save_mode.clone() else {
            return self.accept_current_selection();
        };
        let name = self.save_name_input.text.trim();
        if name.is_empty() {
            self.set_error(FilePickerError::InvalidNewItemName("empty save name".to_string()));
            return FilePickerAction::None;
        }
        if let Err(err) = validate_new_item_name(name) {
            self.set_error(err);
            return FilePickerAction::None;
        }
        let file_name = append_configured_extension(name.to_string(), save_mode.hide_extension.as_deref());
        let path = self.current_dir.join(file_name);
        if path.exists() {
            if save_mode.confirm_overwrite {
                self.pending_save_path = Some(path);
                self.previous_focus = self.focus;
                self.focus = FilePickerFocus::SaveOverwriteConfirm;
                return FilePickerAction::None;
            }
            self.set_error(FilePickerError::DestinationExists(path));
            return FilePickerAction::None;
        }
        FilePickerAction::Selected(path)
    }


    pub(crate) fn confirm_save_overwrite(&mut self) -> FilePickerAction {
        let Some(path) = self.pending_save_path.take() else {
            self.focus = FilePickerFocus::SaveName;
            return FilePickerAction::None;
        };
        FilePickerAction::Selected(path)
    }

    pub(crate) fn cancel_save_overwrite(&mut self) {
        self.pending_save_path = None;
        self.focus = FilePickerFocus::SaveName;
    }

    pub(crate) fn complete_save_name_from_entries(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let prefix = self.save_name_input.text
            [..self.save_name_input.cursor.min(self.save_name_input.text.len())]
            .to_string();
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| !entry.is_dir && entry.name.starts_with(&prefix))
        {
            self.save_name_input = TextInputState::new(entry.name.clone());
        }
    }

    pub fn create_new_file(&mut self) -> bool {
        if !self.operation_policy.allow_new_file {
            self.set_error(FilePickerError::OperationDisabled("new file"));
            return false;
        }
        self.begin_create_name(FilePickerCreateKind::File);
        true
    }

    pub fn create_new_folder(&mut self) -> bool {
        if !self.operation_policy.allow_new_folder {
            self.set_error(FilePickerError::OperationDisabled("new folder"));
            return false;
        }
        self.begin_create_name(FilePickerCreateKind::Folder);
        true
    }

    pub fn try_create_named_item(&mut self, kind: FilePickerCreateKind, name: &str) -> Result<(), FilePickerError> {
        match kind {
            FilePickerCreateKind::File if !self.operation_policy.allow_new_file => {
                return Err(FilePickerError::OperationDisabled("new file"));
            }
            FilePickerCreateKind::Folder if !self.operation_policy.allow_new_folder => {
                return Err(FilePickerError::OperationDisabled("new folder"));
            }
            _ => {}
        }
        validate_new_item_name(name)?;
        let parent = self.pending_name_parent.clone().unwrap_or_else(|| self.current_dir.clone());
        let path = parent.join(name);
        if path.exists() {
            return Err(FilePickerError::DestinationExists(path));
        }
        match kind {
            FilePickerCreateKind::File => fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map(|_| ())
                .map_err(|err| io_error("create file", &path, err))?,
            FilePickerCreateKind::Folder => fs::create_dir(&path).map_err(|err| io_error("create folder", &path, err))?,
        }
        refresh_tree_children(&mut self.tree_nodes, &parent, self.show_hidden);
        if same_path(&parent, &self.current_dir) {
            self.selected = Some(path.clone());
            self.refresh();
            self.select_path_in_entries(&path);
        } else {
            self.select_tree_node_for_current_dir();
        }
        Ok(())
    }

    pub fn cut_current(&mut self) -> bool {
        match self.try_cut_current() {
            Ok(()) => true,
            Err(err) => {
                self.set_error(err);
                false
            }
        }
    }

    pub fn try_cut_current(&mut self) -> Result<(), FilePickerError> {
        if !self.operation_policy.allow_cut {
            return Err(FilePickerError::OperationDisabled("cut"));
        }
        let paths = self.action_paths();
        self.clipboard = Some(
            FilesystemClipboard::new(FilePickerClipboardMode::Cut, paths)
                .ok_or(FilePickerError::NoSelection)?,
        );
        self.clear_error();
        Ok(())
    }

    pub fn copy_current(&mut self) -> bool {
        match self.try_copy_current() {
            Ok(()) => true,
            Err(err) => {
                self.set_error(err);
                false
            }
        }
    }

    pub fn try_copy_current(&mut self) -> Result<(), FilePickerError> {
        if !self.operation_policy.allow_copy {
            return Err(FilePickerError::OperationDisabled("copy"));
        }
        let paths = self.action_paths();
        self.clipboard = Some(
            FilesystemClipboard::new(FilePickerClipboardMode::Copy, paths)
                .ok_or(FilePickerError::NoSelection)?,
        );
        self.clear_error();
        Ok(())
    }

    pub fn paste_clipboard(&mut self) -> bool {
        match self.try_paste_clipboard() {
            Ok(()) => true,
            Err(err) => {
                self.set_error(err);
                false
            }
        }
    }

    pub fn try_paste_clipboard(&mut self) -> Result<(), FilePickerError> {
        let target = self.current_dir.clone();
        self.try_paste_clipboard_to(&target)
    }

    pub(crate) fn try_paste_clipboard_to(
        &mut self,
        target_dir: &Path,
    ) -> Result<(), FilePickerError> {
        if !self.operation_policy.allow_paste {
            return Err(FilePickerError::OperationDisabled("paste"));
        }
        if self.paste_task.as_ref().is_some_and(|task| !task.progress.is_terminal()) {
            return Err(FilePickerError::OperationDisabled("another paste is already running"));
        }
        if !target_dir.is_dir() {
            return Err(FilePickerError::NotADirectory(target_dir.to_path_buf()));
        }
        let clipboard = self.clipboard.clone().ok_or(FilePickerError::ClipboardEmpty)?;
        let plan = plan_filesystem_paste(&clipboard, target_dir)?;
        let control = Arc::new(AtomicU8::new(PASTE_CONTROL_RUNNING));
        let (sender, receiver) = mpsc::channel();
        let mut progress = crate::FileTaskProgressState::new(
            if clipboard.mode() == FilePickerClipboardMode::Cut {
                crate::FileTaskKind::Move
            } else {
                crate::FileTaskKind::Copy
            },
            if clipboard.mode() == FilePickerClipboardMode::Cut {
                "Moving files"
            } else {
                "Copying files"
            },
            self.theme.clone(),
        );
        progress.set_scope(crate::FileTaskScope {
            source_root: None,
            source_summary: format!("{} clipboard item(s)", clipboard.paths().len()),
            destination: Some(target_dir.to_path_buf()),
            destination_summary: Some(target_dir.display().to_string()),
        });
        self.paste_task = Some(PickerPasteTask {
            progress,
            receiver: Some(Arc::new(Mutex::new(receiver))),
            control: Arc::clone(&control),
            clipboard: clipboard.clone(),
            target_dir: target_dir.to_path_buf(),
        });
        let policy = self.operation_policy;
        thread::spawn(move || run_picker_paste_worker(plan, policy, control, sender));
        self.close_menu();
        Ok(())
    }

    pub(crate) fn poll_paste_task(&mut self) {
        let mut finished = None;
        if let Some(task) = self.paste_task.as_mut() {
            let Some(receiver) = task.receiver.as_ref().cloned() else {
                return;
            };
            loop {
                let message = match receiver.lock() {
                    Ok(receiver) => receiver.try_recv(),
                    Err(_) => {
                        task.progress.apply_update(crate::FileTaskProgressUpdate::Failed {
                            status: "paste worker result channel was poisoned".to_string(),
                            totals: task.progress.totals,
                        });
                        task.receiver = None;
                        return;
                    }
                };
                match message {
                    Ok(PickerPasteMessage::Progress(update)) => task.progress.apply_update(update),
                    Ok(PickerPasteMessage::Finished(result)) => {
                        task.receiver = None;
                        finished = Some(result);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        task.receiver = None;
                        if !task.progress.is_terminal() {
                            task.progress.apply_update(crate::FileTaskProgressUpdate::Failed {
                                status: "paste worker disconnected without a terminal result".to_string(),
                                totals: task.progress.totals,
                            });
                        }
                        break;
                    }
                }
            }
        }
        if let Some(result) = finished {
            self.finish_picker_paste(result);
        }
    }

    fn finish_picker_paste(&mut self, result: Result<PasteSuccess, PasteFailure>) {
        let Some(task) = self.paste_task.as_ref() else {
            return;
        };
        let clipboard = task.clipboard.clone();
        let target_dir = task.target_dir.clone();
        let (completed, remaining_sources, warnings, error) = match result {
            Ok(success) => (success.mappings, Vec::new(), success.warnings, None),
            Err(failure) => (
                failure.completed,
                failure.remaining_sources,
                failure.warnings,
                Some(failure.error),
            ),
        };
        let remaining_sources = remaining_sources
            .into_iter()
            .filter(|source| fs::symlink_metadata(source).is_ok())
            .collect::<Vec<_>>();
        let completed_sources = completed
            .iter()
            .map(|mapping| mapping.source.clone())
            .collect::<Vec<_>>();
        let completed_destinations = completed
            .iter()
            .map(|mapping| mapping.destination.clone())
            .collect::<Vec<_>>();
        let remapped_current = if clipboard.mode() == FilePickerClipboardMode::Cut
            && !completed_sources.is_empty()
        {
            FilesystemClipboard::new(FilePickerClipboardMode::Cut, completed_sources.clone())
                .and_then(|completed_clipboard| {
                    for path in self
                        .history_back
                        .iter_mut()
                        .chain(self.history_forward.iter_mut())
                    {
                        if let Some(remapped) = crate::remap_path_after_cut(
                            path,
                            &completed_clipboard,
                            &completed_destinations,
                        ) {
                            *path = remapped;
                        }
                    }
                    crate::remap_path_after_cut(
                        &self.current_dir,
                        &completed_clipboard,
                        &completed_destinations,
                    )
                })
        } else {
            None
        };

        let all_completed = completed.len() == clipboard.paths().len();
        self.clipboard = if all_completed {
            if clipboard.mode() == FilePickerClipboardMode::Copy {
                Some(clipboard.clone())
            } else {
                None
            }
        } else {
            FilesystemClipboard::new(clipboard.mode(), remaining_sources.clone())
        };

        let mut refresh_parents = HashSet::new();
        refresh_parents.insert(target_dir.clone());
        for mapping in &completed {
            if let Some(parent) = mapping.source.parent() {
                refresh_parents.insert(parent.to_path_buf());
            }
            if let Some(parent) = mapping.destination.parent() {
                refresh_parents.insert(parent.to_path_buf());
            }
        }
        for parent in refresh_parents {
            if parent.is_dir() {
                refresh_tree_children(&mut self.tree_nodes, &parent, self.show_hidden);
            }
        }
        if let Some(current) = remapped_current {
            // As with rename, the cut has already committed. Adopt the
            // destination path directly so the UI never retains a vanished
            // current_dir merely because a post-commit stat was transiently
            // unavailable.
            self.adopt_committed_current_dir(current);
        } else {
            self.refresh();
            self.multi_selected = completed_destinations.clone();
            self.selected = completed_destinations.last().cloned();
            if let Some(path) = self.selected.clone() {
                self.select_path_in_entries(&path);
            }
        }

        let warning_summary = warnings
            .iter()
            .map(|warning| warning.message.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        if let Some(task) = self.paste_task.as_mut() {
            let totals = task.progress.totals;
            if let Some(error) = error {
                let mut status = format!(
                    "paste partially completed: {} completed, {} ready to retry; {}",
                    completed.len(),
                    remaining_sources.len(),
                    error.message()
                );
                if !warning_summary.is_empty() {
                    status.push_str(&format!("; committed warnings: {warning_summary}"));
                }
                task.progress.apply_update(crate::FileTaskProgressUpdate::Failed {
                    status,
                    totals,
                });
            } else {
                let status = if warning_summary.is_empty() {
                    format!("pasted {} item(s)", completed.len())
                } else {
                    format!(
                        "pasted {} item(s) with committed warning(s): {}",
                        completed.len(),
                        warning_summary
                    )
                };
                task.progress.apply_update(crate::FileTaskProgressUpdate::Finished {
                    status,
                    totals,
                });
            }
        }
    }

    pub(crate) fn handle_paste_task_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        self.poll_paste_task();
        let Some(task) = self.paste_task.as_mut() else {
            return false;
        };
        let action = task.progress.handle_key(key);
        let clear_task = matches!(action, crate::FileTaskUserAction::Acknowledge)
            && task.progress.is_terminal();
        match action {
            crate::FileTaskUserAction::None | crate::FileTaskUserAction::Acknowledge => {}
            crate::FileTaskUserAction::Pause => {
                task.control.store(PASTE_CONTROL_PAUSED, AtomicOrdering::Release);
                task.progress.apply_update(crate::FileTaskProgressUpdate::Snapshot {
                    phase: crate::FileTaskPhase::Paused,
                    status: "paused".to_string(),
                    current_item: task.progress.current_item.clone(),
                    totals: task.progress.totals,
                    rate_bytes_per_sec: None,
                });
            }
            crate::FileTaskUserAction::Resume => {
                task.control.store(PASTE_CONTROL_RUNNING, AtomicOrdering::Release);
            }
            crate::FileTaskUserAction::SkipCurrent => {
                task.control.store(PASTE_CONTROL_SKIP, AtomicOrdering::Release);
            }
            crate::FileTaskUserAction::Abort => {
                task.control.store(PASTE_CONTROL_ABORT, AtomicOrdering::Release);
            }
            crate::FileTaskUserAction::ChooseConflictResolution(_) => {}
        }
        if clear_task {
            self.paste_task = None;
        }
        true
    }

    pub(crate) fn handle_paste_task_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        area: Rect,
    ) -> bool {
        self.poll_paste_task();
        let Some(task) = self.paste_task.as_mut() else {
            return false;
        };
        let action = task.progress.handle_mouse(mouse, area);
        let clear_task = matches!(action, crate::FileTaskUserAction::Acknowledge)
            && task.progress.is_terminal();
        match action {
            crate::FileTaskUserAction::Pause => {
                task.control.store(PASTE_CONTROL_PAUSED, AtomicOrdering::Release);
            }
            crate::FileTaskUserAction::Resume => {
                task.control.store(PASTE_CONTROL_RUNNING, AtomicOrdering::Release);
            }
            crate::FileTaskUserAction::SkipCurrent => {
                task.control.store(PASTE_CONTROL_SKIP, AtomicOrdering::Release);
            }
            crate::FileTaskUserAction::Abort => {
                task.control.store(PASTE_CONTROL_ABORT, AtomicOrdering::Release);
            }
            _ => {}
        }
        if clear_task {
            self.paste_task = None;
        }
        true
    }

    pub fn request_delete_current(&mut self) -> bool {
        if !self.operation_policy.allow_delete {
            self.set_error(FilePickerError::OperationDisabled("delete"));
            return false;
        }
        let paths = self.action_paths();
        if paths.is_empty() {
            self.set_error(FilePickerError::NoSelection);
            return false;
        }
        self.pending_delete = paths;
        self.capture_pane_focus_for_modal_transition();
        self.focus = FilePickerFocus::DeleteConfirm;
        self.delete_confirm_button = DeleteConfirmButton::Cancel;
        self.clear_error();
        true
    }

    pub fn cancel_delete(&mut self) {
        self.pending_delete.clear();
        self.delete_confirm_button = DeleteConfirmButton::Cancel;
        self.restore_pane_focus();
        self.clear_error();
    }

    pub fn confirm_delete(&mut self) -> bool {
        match self.try_confirm_delete() {
            Ok(()) => true,
            Err(err) => {
                self.set_error(err);
                false
            }
        }
    }

    pub fn try_confirm_delete(&mut self) -> Result<(), FilePickerError> {
        if !self.operation_policy.allow_delete {
            return Err(FilePickerError::OperationDisabled("delete"));
        }
        if self.pending_delete.is_empty() {
            return Err(FilePickerError::NoPendingDelete);
        }

        let refresh_parents = self
            .pending_delete
            .iter()
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .collect::<Vec<_>>();
        let mut delete_error = None;

        while let Some(path) = self.pending_delete.first().cloned() {
            match delete_path(&path, self.operation_policy.delete) {
                Ok(()) => {
                    self.pending_delete.remove(0);
                    self.multi_selected
                        .retain(|selected| !same_path(selected, &path));
                    if self
                        .selected
                        .as_ref()
                        .is_some_and(|selected| same_path(selected, &path))
                    {
                        self.selected = None;
                    }
                }
                Err(error) => {
                    delete_error = Some(error);
                    break;
                }
            }
        }

        repair_navigation_stack(&mut self.history_back);
        repair_navigation_stack(&mut self.history_forward);
        if let Some(repaired_current) = nearest_existing_directory(&self.current_dir) {
            self.current_dir = repaired_current;
        }
        for parent in refresh_parents {
            if parent.is_dir() {
                refresh_tree_children(&mut self.tree_nodes, &parent, self.show_hidden);
            }
        }
        self.refresh();
        self.select_tree_node_for_current_dir();

        if let Some(error) = delete_error {
            return Err(error);
        }
        self.delete_confirm_button = DeleteConfirmButton::Cancel;
        self.restore_pane_focus();
        Ok(())
    }

    pub fn select_path_in_entries(&mut self, path: &Path) -> bool {
        if let Some(index) = self.entries.iter().position(|entry| same_path(&entry.path, path)) {
            self.set_file_cursor(index, self.file_visible_rows());
            true
        } else {
            false
        }
    }

    pub fn visible_total_size(&self) -> u64 {
        self.entries.iter().filter_map(|entry| entry.size).sum()
    }

    pub(crate) fn menu_entries(&self) -> Vec<(&'static str, FilePickerMenuEntry)> {
        use FilePickerContextMenuKind as Kind;
        use FilePickerMenuAction as Action;
        use FilePickerMenuEntry as Entry;
        match self.context_menu_kind {
            Kind::Toolbar => vec![
                ("New      ▸", Entry::NewSubmenu),
                ("Bookmarks", Entry::Action(Action::OpenBookmarks)),
                ("Cut", Entry::Action(Action::Cut)),
                ("Copy", Entry::Action(Action::Copy)),
                ("Paste", Entry::Action(Action::Paste)),
                ("Delete", Entry::Action(Action::Delete)),
            ],
            Kind::Address => vec![
                ("Cut", Entry::Action(Action::TextCut)),
                ("Copy", Entry::Action(Action::TextCopy)),
                ("Paste", Entry::Action(Action::TextPaste)),
            ],
            Kind::Tree => vec![
                ("New      ▸", Entry::NewSubmenu),
                ("Add bookmark", Entry::Action(Action::AddBookmark)),
                ("Cut", Entry::Action(Action::Cut)),
                ("Copy", Entry::Action(Action::Copy)),
                ("Paste", Entry::Action(Action::Paste)),
                ("Rename", Entry::Action(Action::Rename)),
                ("Delete", Entry::Action(Action::Delete)),
            ],
            Kind::File => vec![
                ("Cut", Entry::Action(Action::Cut)),
                ("Copy", Entry::Action(Action::Copy)),
                ("Paste", Entry::Action(Action::Paste)),
                ("Rename", Entry::Action(Action::Rename)),
                ("Duplicate", Entry::Action(Action::Duplicate)),
                ("Delete", Entry::Action(Action::Delete)),
                ("Selection ▸", Entry::SelectionSubmenu),
                ("Open default", Entry::Action(Action::OpenSystemDefault)),
            ],
            Kind::Background => vec![
                ("New      ▸", Entry::NewSubmenu),
                ("Add bookmark", Entry::Action(Action::AddBookmark)),
                ("Bookmarks", Entry::Action(Action::OpenBookmarks)),
                ("Paste", Entry::Action(Action::Paste)),
                ("Selection ▸", Entry::SelectionSubmenu),
            ],
        }
    }

    pub(crate) fn submenu_entries(&self) -> Vec<(&'static str, FilePickerMenuAction)> {
        match self.submenu_kind {
            FilePickerSubmenuKind::New => vec![
                ("File", FilePickerMenuAction::NewFile),
                ("Folder", FilePickerMenuAction::NewFolder),
            ],
            FilePickerSubmenuKind::Selection => vec![
                ("Select All", FilePickerMenuAction::SelectAll),
                ("Invert", FilePickerMenuAction::InvertSelection),
                ("Deselect All", FilePickerMenuAction::DeselectAll),
            ],
        }
    }

    pub(crate) fn is_new_menu_enabled(&self) -> bool {
        self.operation_policy.allow_new_file || self.operation_policy.allow_new_folder
    }

    pub(crate) fn is_menu_action_enabled(&self, action: FilePickerMenuAction) -> bool {
        let action_paths = self.action_paths();
        let single = action_paths.len() == 1;
        match action {
            FilePickerMenuAction::NewFile => self.operation_policy.allow_new_file,
            FilePickerMenuAction::NewFolder => self.operation_policy.allow_new_folder,
            FilePickerMenuAction::Cut => self.operation_policy.allow_cut && !action_paths.is_empty(),
            FilePickerMenuAction::Copy => self.operation_policy.allow_copy && !action_paths.is_empty(),
            FilePickerMenuAction::Rename => single,
            FilePickerMenuAction::Duplicate => !action_paths.is_empty()
                && action_paths.iter().all(|path| path.is_file()),
            FilePickerMenuAction::Delete => self.operation_policy.allow_delete && !action_paths.is_empty(),
            FilePickerMenuAction::Paste => {
                self.operation_policy.allow_paste
                    && self
                        .clipboard
                        .as_ref()
                        .is_some_and(|clipboard| {
                            !clipboard.is_empty()
                                && clipboard.paths().iter().all(|path| path.exists())
                        })
            }
            FilePickerMenuAction::SelectAll => !self.entries.is_empty(),
            FilePickerMenuAction::InvertSelection => !self.entries.is_empty(),
            FilePickerMenuAction::DeselectAll => !self.multi_selected.is_empty(),
            FilePickerMenuAction::TextCut => self.address_input.has_selection(),
            FilePickerMenuAction::TextCopy => self.address_input.has_selection(),
            FilePickerMenuAction::TextPaste => self.address_input.can_paste(),
            FilePickerMenuAction::OpenSystemDefault => single
                && action_paths.first().is_some_and(|path| path.is_file()),
            FilePickerMenuAction::AddBookmark => {
                self.context_menu_kind != FilePickerContextMenuKind::File
            }
            FilePickerMenuAction::OpenBookmarks => true,
        }
    }
}

/// One source-to-destination mapping in a planned or completed paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteMapping {
    pub source: PathBuf,
    pub destination: PathBuf,
}

/// Immutable preflight plan for a clipboard paste. No filesystem mutation has
/// occurred when this value is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastePlan {
    pub mode: FilePickerClipboardMode,
    pub mappings: Vec<PasteMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteWarning {
    pub mapping: PasteMapping,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteSuccess {
    pub mappings: Vec<PasteMapping>,
    pub warnings: Vec<PasteWarning>,
}

/// Structured partial failure. `completed` is authoritative and ordered;
/// `remaining_sources` contains only roots that were not committed and can be
/// used to construct a retry-only clipboard without duplicating prior work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteFailure {
    pub completed: Vec<PasteMapping>,
    pub remaining_sources: Vec<PathBuf>,
    pub warnings: Vec<PasteWarning>,
    pub error: FilePickerError,
}

impl fmt::Display for PasteFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for PasteFailure {}

pub fn plan_filesystem_paste(
    clipboard: &FilesystemClipboard,
    destination_dir: &Path,
) -> Result<PastePlan, FilePickerError> {
    if clipboard.is_empty() {
        return Err(FilePickerError::ClipboardEmpty);
    }
    if !destination_dir.is_dir() {
        return Err(FilePickerError::NotADirectory(destination_dir.to_path_buf()));
    }
    let mut mappings = Vec::with_capacity(clipboard.paths().len());
    let mut reserved = HashSet::new();
    for source in clipboard.paths() {
        if fs::symlink_metadata(source).is_err() {
            return Err(FilePickerError::ClipboardSourceMissing(source.clone()));
        }
        let name = source
            .file_name()
            .ok_or_else(|| FilePickerError::ClipboardPathHasNoFileName(source.clone()))?;
        let destination = unique_path_reserving(&destination_dir.join(name), &reserved);
        reserved.insert(destination.clone());
        mappings.push(PasteMapping {
            source: source.clone(),
            destination,
        });
    }
    Ok(PastePlan {
        mode: clipboard.mode(),
        mappings,
    })
}

/// Paste a shared filesystem clipboard into `destination_dir`.
///
/// This synchronous compatibility entry point returns structured partial
/// accounting. Interactive surfaces should execute the plan on a background
/// worker, but tests and non-interactive callers can still use this function
/// without losing completed mappings or retry state.
pub fn paste_filesystem_clipboard(
    clipboard: &FilesystemClipboard,
    destination_dir: &Path,
    policy: FileOperationPolicy,
) -> Result<PasteSuccess, PasteFailure> {
    let plan = plan_filesystem_paste(clipboard, destination_dir).map_err(|error| PasteFailure {
        completed: Vec::new(),
        remaining_sources: clipboard.paths().to_vec(),
        warnings: Vec::new(),
        error,
    })?;
    let mut completed = Vec::with_capacity(plan.mappings.len());
    let mut warnings = Vec::new();
    for (index, mapping) in plan.mappings.iter().enumerate() {
        let result = match plan.mode {
            FilePickerClipboardMode::Cut => {
                move_path_with_policy(&mapping.source, &mapping.destination, policy)
            }
            FilePickerClipboardMode::Copy => {
                safe_copy_path(&mapping.source, &mapping.destination, policy)
            }
        };
        match result {
            Ok(()) => completed.push(mapping.clone()),
            Err(FilePickerError::OperationCommittedWithWarning { message, .. }) => {
                completed.push(mapping.clone());
                warnings.push(PasteWarning {
                    mapping: mapping.clone(),
                    message,
                });
            }
            Err(error) => {
                let remaining_sources = plan.mappings[index..]
                    .iter()
                    .map(|mapping| mapping.source.clone())
                    .filter(|source| fs::symlink_metadata(source).is_ok())
                    .collect();
                return Err(PasteFailure {
                    completed,
                    remaining_sources,
                    warnings,
                    error,
                });
            }
        }
    }
    Ok(PasteSuccess { mappings: completed, warnings })
}

fn run_picker_paste_worker(
    plan: PastePlan,
    policy: FileOperationPolicy,
    control: Arc<AtomicU8>,
    sender: mpsc::Sender<PickerPasteMessage>,
) {
    let started = Instant::now();
    let mut last_update = Instant::now() - Duration::from_secs(1);
    let mut totals = crate::ProgressTotals::default();
    // Recursive work is discovered lazily. Keep the item total unknown rather
    // than displaying a mathematically false top-level-root denominator.
    totals.items_total = None;
    totals.item_unit = crate::ProgressUnit::Items;
    let mut progress = |source: &Path, destination: &Path, bytes: u64, completed: bool| {
        loop {
            match control.load(AtomicOrdering::Acquire) {
                PASTE_CONTROL_RUNNING => break,
                PASTE_CONTROL_PAUSED => {
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                PASTE_CONTROL_ABORT => return Err(FilePickerError::OperationCancelled),
                PASTE_CONTROL_SKIP => {
                    control.store(PASTE_CONTROL_RUNNING, AtomicOrdering::Release);
                    return Err(FilePickerError::OperationSkipped);
                }
                _ => break,
            }
        }
        totals.bytes_done = totals.bytes_done.saturating_add(bytes);
        if completed {
            totals.items_done = totals.items_done.saturating_add(1);
            totals.completed = totals.completed.saturating_add(1);
        }
        let now = Instant::now();
        if completed || now.saturating_duration_since(last_update) >= Duration::from_millis(80) {
            last_update = now;
            let elapsed = now.saturating_duration_since(started).as_secs().max(1);
            let _ = sender.send(PickerPasteMessage::Progress(
                crate::FileTaskProgressUpdate::Snapshot {
                    phase: crate::FileTaskPhase::Running,
                    status: format!("Processing {}", source.display()),
                    current_item: Some(crate::ProgressItem {
                        label: source
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| source.display().to_string()),
                        source: Some(source.to_path_buf()),
                        destination: Some(destination.to_path_buf()),
                        bytes_done: 0,
                        bytes_total: None,
                    }),
                    totals,
                    rate_bytes_per_sec: Some(totals.bytes_done / elapsed),
                },
            ));
        }
        Ok(())
    };
    let result = execute_paste_plan_progress(&plan, policy, &mut progress);
    let _ = sender.send(PickerPasteMessage::Finished(result));
}

fn execute_paste_plan_progress(
    plan: &PastePlan,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
) -> Result<PasteSuccess, PasteFailure> {
    let mut completed = Vec::new();
    let mut retry_sources = Vec::new();
    let mut warnings = Vec::new();
    for (index, mapping) in plan.mappings.iter().enumerate() {
        let result = match plan.mode {
            FilePickerClipboardMode::Cut => move_path_with_policy_progress(
                &mapping.source,
                &mapping.destination,
                policy,
                progress,
            ),
            FilePickerClipboardMode::Copy => safe_copy_path_progress(
                &mapping.source,
                &mapping.destination,
                policy,
                progress,
            ),
        };
        match result {
            Ok(()) => completed.push(mapping.clone()),
            Err(FilePickerError::OperationCommittedWithWarning { message, .. }) => {
                completed.push(mapping.clone());
                warnings.push(PasteWarning {
                    mapping: mapping.clone(),
                    message,
                });
            }
            Err(FilePickerError::OperationSkipped) => {
                if fs::symlink_metadata(&mapping.source).is_ok() {
                    retry_sources.push(mapping.source.clone());
                }
            }
            Err(error) => {
                if fs::symlink_metadata(&mapping.source).is_ok() {
                    retry_sources.push(mapping.source.clone());
                }
                retry_sources.extend(
                    plan.mappings[index + 1..]
                        .iter()
                        .map(|mapping| mapping.source.clone())
                        .filter(|source| fs::symlink_metadata(source).is_ok()),
                );
                return Err(PasteFailure {
                    completed,
                    remaining_sources: retry_sources,
                    warnings,
                    error,
                });
            }
        }
    }
    if retry_sources.is_empty() {
        Ok(PasteSuccess { mappings: completed, warnings })
    } else {
        Err(PasteFailure {
            completed,
            remaining_sources: retry_sources,
            warnings,
            error: FilePickerError::OperationSkipped,
        })
    }
}

/// Duplicate files in place, returning the created paths.
///
/// Directory duplication is intentionally rejected in both surfaces until a
/// cancellable recursive-copy job is available; this keeps large operations
/// off the UI thread and preserves picker/Browse parity.
pub fn duplicate_files_in_place(
    paths: &[PathBuf],
    policy: FileOperationPolicy,
) -> Result<Vec<PathBuf>, FilePickerError> {
    if paths.is_empty() {
        return Err(FilePickerError::NoSelection);
    }
    if paths.iter().any(|path| !path.is_file()) {
        return Err(FilePickerError::WrongSelectionMode(
            "Duplicate supports files only",
        ));
    }

    // Resolve every destination before mutating the filesystem. This keeps
    // naming deterministic and prevents a late collision from leaving an
    // avoidable partial result.
    let mut reserved = HashSet::new();
    let mut plans = Vec::with_capacity(paths.len());
    for source in paths {
        let candidate = duplicate_candidate_path(source);
        let destination = unique_path_reserving(&candidate, &reserved);
        reserved.insert(destination.clone());
        plans.push((source.clone(), destination));
    }

    let mut completed: Vec<PathBuf> = Vec::with_capacity(plans.len());
    for (source, destination) in plans {
        if let Err(copy_error) = safe_copy_path(&source, &destination, policy) {
            // Duplicates are newly-created outputs, so rollback is safe and
            // materially stronger than exposing a half-completed bulk action.
            for created in completed.iter().rev() {
                if let Err(cleanup_error) = fs::remove_file(created) {
                    return Err(FilePickerError::Io {
                        op: "rollback duplicate",
                        path: created.clone(),
                        message: format!(
                            "{}; additionally could not remove completed duplicate: {}",
                            copy_error.message(),
                            cleanup_error
                        ),
                    });
                }
            }
            return Err(copy_error);
        }
        completed.push(destination);
    }
    Ok(completed)
}

fn duplicate_candidate_path(source: &Path) -> PathBuf {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let name = source.file_name().and_then(OsStr::to_str).unwrap_or("item");
    let stem = source.file_stem().and_then(OsStr::to_str).unwrap_or(name);
    let candidate_name = match source.extension().and_then(OsStr::to_str) {
        Some(extension) => format!("{stem}-copy.{extension}"),
        None => format!("{stem}-copy"),
    };
    parent.join(candidate_name)
}

type FileOperationProgress<'a> = dyn FnMut(&Path, &Path, u64, bool) -> Result<(), FilePickerError> + 'a;

fn move_path_with_policy(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
) -> Result<(), FilePickerError> {
    let mut progress = |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
    move_path_with_policy_progress(source, destination, policy, &mut progress)
}

fn move_path_with_policy_progress(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
) -> Result<(), FilePickerError> {
    progress(source, destination, 0, false)?;
    let source_snapshot = crate::snapshot_path(source)
        .map_err(|error| io_error("capture move source identity", source, error))?;
    match rename_no_replace(source, destination) {
        Ok(()) => {
            let identity_error = match crate::snapshot_path(destination) {
                Ok(moved_snapshot) => source_snapshot.verify_same_identity(&moved_snapshot).err(),
                Err(error) => Some(format!("could not re-identify moved source: {error}")),
            };
            if let Some(message) = identity_error {
                // The rename has committed, but the destination pathname no
                // longer proves ownership of the moved object. Do not attempt
                // pathname rollback: that could move an unrelated replacement
                // into the source name. Leave both names untouched and surface
                // the unproven committed state.
                return Err(committed_operation_warning(
                    source,
                    destination,
                    format!(
                        "move committed, but the destination no longer proves it names the captured source object: {message}"
                    ),
                ));
            }
            let mut warnings = Vec::new();
            if let Some(parent) = destination.parent() {
                if let Err(err) = sync_directory(parent) {
                    warnings.push(format!(
                        "destination parent directory synchronization failed: {err}"
                    ));
                }
            }
            if let Some(parent) = source.parent() {
                if destination.parent() != Some(parent) {
                    if let Err(err) = sync_directory(parent) {
                        warnings.push(format!(
                            "source parent directory synchronization failed: {err}"
                        ));
                    }
                }
            }
            if let Err(err) = progress(source, destination, 0, true) {
                warnings.push(format!(
                    "progress control changed after the atomic move committed: {}",
                    err.message()
                ));
            }
            if warnings.is_empty() {
                Ok(())
            } else {
                Err(committed_operation_warning(
                    source,
                    destination,
                    warnings.join("; "),
                ))
            }
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            Err(FilePickerError::DestinationExists(destination.to_path_buf()))
        }
        Err(err) if is_cross_device_error(&err) => match policy.cross_device_cut {
            CrossDeviceCutPolicy::Reject => Err(FilePickerError::CrossDeviceMoveRejected {
                source: source.to_path_buf(),
                destination: destination.to_path_buf(),
            }),
            CrossDeviceCutPolicy::CopyThenDelete => {
                copy_then_delete_progress(source, destination, policy, progress)
            }
        },
        Err(err) if err.kind() == io::ErrorKind::Unsupported => match policy.cross_device_cut {
            CrossDeviceCutPolicy::Reject => Err(io_error("move without replacement", source, err)),
            CrossDeviceCutPolicy::CopyThenDelete => {
                copy_then_delete_progress(source, destination, policy, progress)
            }
        },
        Err(err) => Err(io_error("move", source, err)),
    }
}

fn copy_then_delete_progress(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
) -> Result<(), FilePickerError> {
    let mut manifest_control_error = None;
    let manifest_result = crate::capture_manifest_with_cancel(source, |path| {
        if manifest_control_error.is_some() {
            return false;
        }
        match progress(path, destination, 0, false) {
            Ok(()) => true,
            Err(error) => {
                manifest_control_error = Some(error);
                false
            }
        }
    });
    if let Some(error) = manifest_control_error {
        return Err(error);
    }
    let manifest = manifest_result.map_err(|message| FilePickerError::Io {
        op: "capture stable move source",
        path: source.to_path_buf(),
        message,
    })?;

    match safe_copy_path_progress(source, destination, policy, progress) {
        Ok(()) => {}
        Err(FilePickerError::OperationCommittedWithWarning { message, .. }) => {
            return Err(committed_operation_warning(
                source,
                destination,
                format!("destination published; source retained because {message}"),
            ));
        }
        Err(error) => return Err(error),
    }
    let mut destination_verification_control_error = None;
    let destination_verification = manifest.capture_verified_copy_at_with_cancel(destination, |path| {
        if destination_verification_control_error.is_some() {
            return false;
        }
        match progress(path, destination, 0, false) {
            Ok(()) => true,
            Err(error) => {
                destination_verification_control_error = Some(error);
                false
            }
        }
    });
    if let Some(error) = destination_verification_control_error {
        return Err(committed_operation_warning(
            source,
            destination,
            format!(
                "destination published; source retained because destination verification was interrupted: {}",
                error.message()
            ),
        ));
    }
    let destination_manifest = match destination_verification {
        Ok(destination_manifest) => destination_manifest,
        Err(error) => {
            return Err(committed_operation_warning(
                source,
                destination,
                format!("destination published, but content/identity verification failed; source retained: {error}"),
            ));
        }
    };

    let quarantine = match quarantine_picker_source(source) {
        Ok(path) => path,
        Err(error) => {
            return Err(committed_operation_warning(
                source,
                destination,
                format!("destination published; source retained because safe cleanup could not begin: {error}"),
            ))
        }
    };
    let mut source_verification_control_error = None;
    let source_verification = manifest.verify_at_with_cancel(&quarantine, |path| {
        if source_verification_control_error.is_some() {
            return false;
        }
        match progress(path, destination, 0, false) {
            Ok(()) => true,
            Err(error) => {
                source_verification_control_error = Some(error);
                false
            }
        }
    });
    if let Some(error) = source_verification_control_error {
        let recovery = restore_picker_quarantine(&quarantine, source);
        return Err(committed_operation_warning(
            source,
            destination,
            format!(
                "destination published; source verification was interrupted before cleanup and no source object was deleted: {}; {recovery}",
                error.message()
            ),
        ));
    }
    if let Err(error) = source_verification {
        let recovery = restore_picker_quarantine(&quarantine, source);
        return Err(committed_operation_warning(
            source,
            destination,
            format!(
                "destination published, but source identity/content changed before cleanup; no source object was deleted: {error}; {recovery}"
            ),
        ));
    }

    if let Err(error) = delete_verified_quarantine_progress(
        &quarantine,
        &quarantine,
        destination,
        policy.delete,
        &manifest,
        &destination_manifest,
        progress,
    ) {
        return Err(committed_operation_warning(
            source,
            destination,
            format!(
                "destination published, but verified source cleanup did not complete; remnants remain quarantined at {}: {}",
                quarantine.display(),
                error.message()
            ),
        ));
    }
    if let Some(container) = quarantine.parent() {
        if let Err(err) = fs::remove_dir(container) {
            if err.kind() != io::ErrorKind::NotFound {
                return Err(committed_operation_warning(
                    source,
                    destination,
                    format!(
                        "verified source removed, but empty quarantine {} could not be removed: {err}",
                        container.display()
                    ),
                ));
            }
        }
    }
    if let Some(parent) = source.parent() {
        if let Err(err) = sync_directory(parent) {
            return Err(committed_operation_warning(
                source,
                destination,
                format!("verified source removed, but source-parent synchronization failed: {err}"),
            ));
        }
    }
    Ok(())
}

static PICKER_SOURCE_QUARANTINE_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn quarantine_picker_source(source: &Path) -> Result<PathBuf, String> {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();
    for _ in 0..256 {
        let nonce = PICKER_SOURCE_QUARANTINE_COUNTER
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let container = parent.join(format!(
            ".tui-file-picker-source-quarantine-{pid}-{nonce}"
        ));
        match create_private_directory(&container) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "could not reserve a private source quarantine beside {}: {error}",
                    source.display()
                ))
            }
        }
        let quarantine = container.join("payload");
        match rename_no_replace(source, &quarantine) {
            Ok(()) => return Ok(quarantine),
            Err(error) => {
                let _ = fs::remove_dir(&container);
                return Err(format!(
                    "could not atomically quarantine {} without replacing another path: {error}",
                    source.display()
                ));
            }
        }
    }
    Err(format!(
        "could not allocate a private quarantine beside {}",
        source.display()
    ))
}

fn try_restore_picker_quarantine(quarantine: &Path, original: &Path) -> (bool, String) {
    match rename_no_replace(quarantine, original) {
        Ok(()) => {
            let mut details = vec![format!("source restored to {}", original.display())];
            if let Some(container) = quarantine.parent() {
                if let Err(error) = fs::remove_dir(container) {
                    if error.kind() != io::ErrorKind::NotFound {
                        details.push(format!(
                            "empty quarantine {} could not be removed: {error}",
                            container.display()
                        ));
                    }
                }
            }
            if let Some(parent) = original.parent() {
                if let Err(error) = sync_directory(parent) {
                    details.push(format!(
                        "restored source parent could not be synchronized: {error}"
                    ));
                }
            }
            (true, details.join("; "))
        }
        Err(error) => (
            false,
            format!(
                "source retained at {} because restoration to {} failed: {error}",
                quarantine.display(),
                original.display()
            ),
        ),
    }
}

fn restore_picker_quarantine(quarantine: &Path, original: &Path) -> String {
    try_restore_picker_quarantine(quarantine, original).1
}

fn delete_verified_quarantine_progress(
    root: &Path,
    path: &Path,
    destination: &Path,
    policy: DeletePolicy,
    manifest: &crate::SourceManifest,
    destination_manifest: &crate::DestinationManifest,
    progress: &mut FileOperationProgress<'_>,
) -> Result<(), FilePickerError> {
    progress(path, destination, 0, false)?;
    let relative = path.strip_prefix(root).map_err(|_| FilePickerError::Io {
        op: "verify quarantined source path",
        path: path.to_path_buf(),
        message: "quarantine traversal escaped its root".to_string(),
    })?;
    manifest
        .verify_entry_at(relative, path)
        .map_err(|message| FilePickerError::Io {
            op: "verify quarantined source entry",
            path: path.to_path_buf(),
            message,
        })?;
    let expected = manifest.expected_snapshot(relative).ok_or_else(|| FilePickerError::Io {
        op: "verify quarantined source manifest",
        path: path.to_path_buf(),
        message: "unplanned source entry appeared during cleanup".to_string(),
    })?;

    if expected.kind() == crate::SourceKind::Directory {
        match policy {
            DeletePolicy::FilesAndEmptyDirectories => {}
            DeletePolicy::Recursive => {
                let mut children = fs::read_dir(path)
                    .map_err(|err| io_error("read quarantined source directory", path, err))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|err| io_error("read quarantined source entry", path, err))?;
                children.sort_by_key(|entry| entry.file_name());
                for child in children {
                    delete_verified_quarantine_progress(
                        root,
                        &child.path(),
                        destination,
                        policy,
                        manifest,
                        destination_manifest,
                        progress,
                    )?;
                }
            }
        }
    }

    if expected.kind() != crate::SourceKind::Directory {
        manifest
            .verify_entry_at(relative, path)
            .map_err(|message| FilePickerError::Io {
                op: "verify source content and identity immediately before deletion",
                path: path.to_path_buf(),
                message,
            })?;
    }

    let immediately_before_delete = crate::snapshot_path(path)
        .map_err(|error| io_error("re-identify source immediately before deletion", path, error))?;
    expected
        .verify_same_identity(&immediately_before_delete)
        .map_err(|message| FilePickerError::Io {
            op: "verify source identity immediately before deletion",
            path: path.to_path_buf(),
            message,
        })?;

    // Make the corresponding destination proof the final gate before source
    // removal. The source lives inside a private quarantine at this point; if
    // the destination disappeared, changed contents, or changed pathname
    // identity after the earlier whole-tree verification, cleanup stops here.
    let destination_path = if relative.as_os_str().is_empty() {
        destination.to_path_buf()
    } else {
        destination.join(relative)
    };
    let mut verification_interruption = None;
    let destination_verification = destination_manifest.verify_entry_at_with_cancel(
        manifest,
        relative,
        &destination_path,
        &mut |verified_path| match progress(path, verified_path, 0, false) {
            Ok(()) => true,
            Err(error) => {
                verification_interruption = Some(error);
                false
            }
        },
    );
    if let Some(error) = verification_interruption {
        return Err(error);
    }
    destination_verification.map_err(|message| FilePickerError::Io {
        op: "verify destination immediately before source deletion",
        path: destination_path,
        message,
    })?;

    if immediately_before_delete.kind() == crate::SourceKind::Directory {
        fs::remove_dir(path)
            .map_err(|err| io_error("delete quarantined source directory", path, err))?;
    } else {
        fs::remove_file(path)
            .map_err(|err| io_error("delete quarantined source file", path, err))?;
    }
    progress(path, destination, 0, true)
}

fn committed_operation_warning(
    source: &Path,
    destination: &Path,
    message: String,
) -> FilePickerError {
    FilePickerError::OperationCommittedWithWarning {
        source: source.to_path_buf(),
        destination: destination.to_path_buf(),
        message,
    }
}

pub(crate) fn intersect_rect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let bottom = a.y.saturating_add(a.height).min(b.y.saturating_add(b.height));
    if right > left && bottom > top {
        Some(Rect::new(left, top, right - left, bottom - top))
    } else {
        None
    }
}

fn validate_new_item_name(name: &str) -> Result<(), FilePickerError> {
    crate::validate_file_name(name)
        .map(|_| ())
        .map_err(|_| FilePickerError::InvalidNewItemName(name.to_string()))
}

fn normalize_start_dir(path: &Path) -> PathBuf {
    let expanded = expand_user_path(&path.to_string_lossy());
    if expanded.is_dir() {
        return expanded;
    }
    if let Some(parent) = expanded.parent().filter(|parent| parent.is_dir()) {
        return parent.to_path_buf();
    }
    home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn expand_user_path(input: &str) -> PathBuf {
    if input == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(input));
    }
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

fn strip_configured_extension(name: &str, extension: Option<&str>) -> String {
    let Some(extension) = extension.filter(|extension| !extension.is_empty()) else {
        return name.to_string();
    };
    name.strip_suffix(extension).unwrap_or(name).to_string()
}

fn append_configured_extension(mut name: String, extension: Option<&str>) -> String {
    let Some(extension) = extension.filter(|extension| !extension.is_empty()) else {
        return name;
    };
    if !name.ends_with(extension) {
        name.push_str(extension);
    }
    name
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

fn normalize_path_for_navigation(current_dir: &Path, path: &Path) -> PathBuf {
    let expanded = expand_user_path(&path.to_string_lossy());
    if expanded.is_absolute() {
        expanded
    } else {
        current_dir.join(expanded)
    }
}

fn nearest_existing_directory(path: &Path) -> Option<PathBuf> {
    let mut candidate = path.to_path_buf();
    loop {
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !candidate.pop() {
            return None;
        }
    }
}

fn repair_navigation_stack(stack: &mut Vec<PathBuf>) {
    let mut repaired: Vec<PathBuf> = Vec::with_capacity(stack.len());
    for path in stack.drain(..) {
        let Some(path) = nearest_existing_directory(&path) else {
            continue;
        };
        if repaired
            .last()
            .is_some_and(|previous| same_path(previous, &path))
        {
            continue;
        }
        repaired.push(path);
    }
    *stack = repaired;
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let a_canon = a.canonicalize();
    let b_canon = b.canonicalize();
    matches!((a_canon, b_canon), (Ok(a), Ok(b)) if a == b)
}

fn compare_entries(a: &FilePickerEntry, b: &FilePickerEntry, sort_key: FilePickerSortKey, reverse: bool) -> Ordering {
    let folder_order = b.is_dir.cmp(&a.is_dir);
    if folder_order != Ordering::Equal {
        return folder_order;
    }
    let ordering = match sort_key {
        FilePickerSortKey::Name => cmp_name(&a.name, &b.name),
        FilePickerSortKey::Type => a.file_type.cmp(&b.file_type).then_with(|| cmp_name(&a.name, &b.name)),
        FilePickerSortKey::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)).then_with(|| cmp_name(&a.name, &b.name)),
        FilePickerSortKey::Modified => a.modified.cmp(&b.modified).then_with(|| cmp_name(&a.name, &b.name)),
    };
    if reverse { ordering.reverse() } else { ordering }
}

fn cmp_name(a: &str, b: &str) -> Ordering {
    a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()).then_with(|| a.cmp(b))
}

fn file_type_label(path: PathBuf, is_dir: bool, is_symlink: bool) -> String {
    if is_symlink {
        return "Symlink".to_string();
    }
    if is_dir {
        return "Folder".to_string();
    }
    match path.extension().and_then(OsStr::to_str).map(|ext| ext.to_ascii_lowercase()) {
        Some(ext) if matches!(ext.as_str(), "rs") => "Rust source".to_string(),
        Some(ext) if matches!(ext.as_str(), "toml") => "TOML".to_string(),
        Some(ext) if matches!(ext.as_str(), "md") => "Markdown".to_string(),
        Some(ext) if matches!(ext.as_str(), "txt") => "Text".to_string(),
        Some(ext) if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp") => ext.to_ascii_uppercase(),
        Some(ext) if matches!(ext.as_str(), "flac" | "wav" | "aiff" | "aif" | "wv" | "mp3" | "m4a" | "aac" | "ogg" | "opus") => ext.to_ascii_uppercase(),
        Some(ext) => ext.to_ascii_uppercase(),
        None => "File".to_string(),
    }
}

fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(OsStr::to_str).unwrap_or("item");
    let extension = path.extension().and_then(OsStr::to_str);
    for index in 2..10_000usize {
        let name = match extension {
            Some(extension) => format!("{stem} {index}.{extension}"),
            None => format!("{stem} {index}"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem} copy"))
}

fn unique_path_reserving(path: &Path, reserved: &HashSet<PathBuf>) -> PathBuf {
    if !path.exists() && !reserved.contains(path) {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(OsStr::to_str).unwrap_or("item");
    let extension = path.extension().and_then(OsStr::to_str);
    for index in 2..10_000usize {
        let name = match extension {
            Some(extension) => format!("{stem} {index}.{extension}"),
            None => format!("{stem} {index}"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() && !reserved.contains(&candidate) {
            return candidate;
        }
    }
    parent.join(format!("{stem} copy"))
}

#[cfg(target_os = "linux")]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let old = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("source path contains NUL: {}", source.display()),
        )
    })?;
    let new = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("destination path contains NUL: {}", destination.display()),
        )
    })?;
    const RENAME_NOREPLACE: libc::c_uint = 1;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            old.as_ptr(),
            libc::AT_FDCWD,
            new.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EEXIST) => Err(io::Error::new(io::ErrorKind::AlreadyExists, error)),
        Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is not supported by this kernel or filesystem",
        )),
        _ => Err(error),
    }
}

#[cfg(windows)]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    // std::fs::rename maps to a no-replace move on Windows when the destination
    // already exists.
    fs::rename(source, destination)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    let _ = (source, destination);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace rename is unavailable on this target; refusing an unsafe fallback",
    ))
}

fn safe_copy_path(src: &Path, dst: &Path, policy: FileOperationPolicy) -> Result<(), FilePickerError> {
    let mut progress = |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
    safe_copy_path_progress(src, dst, policy, &mut progress)
}

fn safe_copy_path_progress(
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
) -> Result<(), FilePickerError> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    if fs::symlink_metadata(dst).is_ok() {
        return Err(FilePickerError::DestinationExists(dst.to_path_buf()));
    }
    let staging_container = create_unique_staging_directory(parent)?;
    let staging = staging_container.join("payload");
    let mut visited = HashSet::new();
    let mut metadata_warnings = Vec::new();
    let mut publication_identity_warning = None;
    let result = copy_path_to_staging(
        src,
        &staging,
        policy,
        &mut visited,
        progress,
        &mut metadata_warnings,
    )
    .and_then(|()| {
        progress(src, dst, 0, false)?;
        let staged_identity = crate::snapshot_path(&staging)
            .map_err(|error| io_error("identify completed staged copy", &staging, error))?;
        rename_no_replace(&staging, dst).map_err(|err| {
            if err.kind() == io::ErrorKind::AlreadyExists {
                FilePickerError::DestinationExists(dst.to_path_buf())
            } else {
                io_error("commit staged copy", dst, err)
            }
        })?;
        publication_identity_warning = match crate::snapshot_path(dst) {
            Ok(destination_identity) => staged_identity
                .verify_same_identity(&destination_identity)
                .err()
                .map(|message| format!(
                    "final destination no longer names the staged object published by this operation: {message}"
                )),
            Err(error) => Some(format!(
                "could not re-identify the final destination after publication: {error}"
            )),
        };
        Ok(())
    });
    match result {
        Ok(()) => {
            let mut warnings = metadata_warnings;
            if let Some(warning) = publication_identity_warning {
                warnings.push(warning);
            }
            if let Err(err) = fs::remove_dir(&staging_container) {
                if err.kind() != io::ErrorKind::NotFound {
                    warnings.push(format!(
                        "could not remove empty staging directory {}: {err}",
                        staging_container.display()
                    ));
                }
            }
            if let Err(err) = sync_directory(parent) {
                warnings.push(format!(
                    "destination directory synchronization failed: {err}"
                ));
            }
            if let Err(err) = progress(src, dst, 0, true) {
                warnings.push(format!(
                    "progress control changed after copy publication: {}",
                    err.message()
                ));
            }
            if warnings.is_empty() {
                Ok(())
            } else {
                Err(committed_operation_warning(src, dst, warnings.join("; ")))
            }
        }
        Err(err) => {
            cleanup_staging(&staging_container);
            Err(err)
        }
    }
}

fn copy_path_to_staging(
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    visited: &mut HashSet<PathBuf>,
    progress: &mut FileOperationProgress<'_>,
    metadata_warnings: &mut Vec<String>,
) -> Result<(), FilePickerError> {
    progress(src, dst, 0, false)?;
    let source_snapshot = crate::snapshot_path(src)
        .map_err(|err| io_error("capture source identity", src, err))?;
    let symlink_metadata = fs::symlink_metadata(src).map_err(|err| io_error("read metadata", src, err))?;
    if symlink_metadata.file_type().is_symlink() {
        match policy.symlink_copy {
            SymlinkCopyPolicy::Reject => return Err(FilePickerError::SymlinkRejected(src.to_path_buf())),
            SymlinkCopyPolicy::FollowTarget => {}
        }
    }
    let metadata = fs::metadata(src).map_err(|err| io_error("read target metadata", src, err))?;
    if metadata.is_dir() {
        let src_canon = src.canonicalize().map_err(|err| io_error("canonicalize source", src, err))?;
        let dst_parent_canon = dst
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()
            .map_err(|err| io_error("canonicalize destination parent", dst, err))?;
        if dst_parent_canon.starts_with(&src_canon) || !visited.insert(src_canon.clone()) {
            return Err(FilePickerError::CopyCycleRejected { source: src_canon, destination: dst.to_path_buf() });
        }
        fs::create_dir(dst).map_err(|err| io_error("create staged directory", dst, err))?;
        let result = copy_dir_contents(
            src,
            dst,
            policy,
            visited,
            progress,
            metadata_warnings,
        )
        .and_then(|()| {
            match crate::verify_path(src, &source_snapshot) {
                Ok(()) => metadata_warnings.extend(
                    preserve_copied_metadata(src, dst, &metadata)
                        .into_iter()
                        .map(|warning| format!("{}: {warning}", src.display())),
                ),
                Err(message) => metadata_warnings.push(format!(
                    "{}: source changed before directory metadata preservation; metadata was not reapplied: {message}",
                    src.display()
                )),
            }
            sync_directory(dst).map_err(|err| io_error("sync copied directory", dst, err))
        });
        visited.remove(&src_canon);
        result?;
        progress(src, dst, 0, true)
    } else if metadata.is_file() {
        let expected_snapshot = if source_snapshot.kind() == crate::SourceKind::File {
            Some(&source_snapshot)
        } else {
            None
        };
        copy_regular_file(
            src,
            dst,
            &metadata,
            expected_snapshot,
            progress,
            metadata_warnings,
        )?;
        progress(src, dst, 0, true)
    } else {
        Err(FilePickerError::Io {
            op: "copy special file",
            path: src.to_path_buf(),
            message: "special files are not supported".to_string(),
        })
    }
}

fn copy_dir_contents(
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    visited: &mut HashSet<PathBuf>,
    progress: &mut FileOperationProgress<'_>,
    metadata_warnings: &mut Vec<String>,
) -> Result<(), FilePickerError> {
    for entry in fs::read_dir(src).map_err(|err| io_error("read directory", src, err))? {
        progress(src, dst, 0, false)?;
        let entry = entry.map_err(|err| io_error("read directory entry", src, err))?;
        let child_src = entry.path();
        let child_dst = dst.join(entry.file_name());
        copy_path_to_staging(
            &child_src,
            &child_dst,
            policy,
            visited,
            progress,
            metadata_warnings,
        )?;
    }
    Ok(())
}

fn copy_regular_file(
    src: &Path,
    dst: &Path,
    _metadata: &fs::Metadata,
    expected_snapshot: Option<&crate::SourceSnapshot>,
    progress: &mut FileOperationProgress<'_>,
    metadata_warnings: &mut Vec<String>,
) -> Result<(), FilePickerError> {
    let mut source = fs::File::open(src).map_err(|err| io_error("open source file", src, err))?;
    let opened_snapshot = crate::snapshot_open_file(&source)
        .map_err(|err| io_error("capture opened source identity", src, err))?;
    if let Some(expected) = expected_snapshot {
        expected
            .verify_same_object_and_version(&opened_snapshot)
            .map_err(|message| FilePickerError::Io {
                op: "verify source identity before copying",
                path: src.to_path_buf(),
                message,
            })?;
    }
    let opened_metadata = source
        .metadata()
        .map_err(|err| io_error("read opened source metadata", src, err))?;
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
        .map_err(|err| io_error("create staged file", dst, err))?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut copied = 0u64;
    loop {
        progress(src, dst, 0, false)?;
        let read = source.read(&mut buffer).map_err(|err| io_error("read source file", src, err))?;
        if read == 0 {
            break;
        }
        destination
            .write_all(&buffer[..read])
            .map_err(|err| io_error("write staged file", dst, err))?;
        copied = copied.saturating_add(read as u64);
        progress(src, dst, read as u64, false)?;
    }
    let final_snapshot = crate::snapshot_open_file(&source)
        .map_err(|err| io_error("re-identify opened source", src, err))?;
    opened_snapshot
        .verify_same_object_and_version(&final_snapshot)
        .map_err(|message| FilePickerError::Io {
            op: "verify source remained stable while copying",
            path: src.to_path_buf(),
            message,
        })?;
    if copied != opened_metadata.len() {
        return Err(FilePickerError::Io {
            op: "verify copied source length",
            path: src.to_path_buf(),
            message: format!(
                "source length changed while copying (expected {}, copied {})",
                opened_metadata.len(), copied
            ),
        });
    }
    destination
        .sync_all()
        .map_err(|err| io_error("sync staged file", dst, err))?;
    metadata_warnings.extend(
        crate::preserve_open_file_metadata(&source, &destination)
            .into_iter()
            .map(|warning| format!("{}: {warning}", src.display())),
    );
    destination
        .sync_all()
        .map_err(|err| io_error("sync staged file metadata", dst, err))
}

fn preserve_copied_metadata(
    src: &Path,
    dst: &Path,
    metadata: &fs::Metadata,
) -> Vec<String> {
    let mut warnings = Vec::new();
    // Ownership changes can clear set-ID mode bits, so apply Unix ownership,
    // timestamps, and xattrs before restoring the final permission mode.
    warnings.extend(preserve_unix_metadata(src, dst, metadata));
    if let Err(err) = fs::set_permissions(dst, metadata.permissions()) {
        warnings.push(format!("permissions: {err}"));
    }
    warnings
}

#[cfg(unix)]
fn preserve_unix_metadata(
    src: &Path,
    dst: &Path,
    metadata: &fs::Metadata,
) -> Vec<String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let mut warnings = Vec::new();
    let destination = match CString::new(dst.as_os_str().as_bytes()) {
        Ok(destination) => destination,
        Err(_) => return vec!["destination path contains NUL".to_string()],
    };
    let chown_result = unsafe { libc::chown(destination.as_ptr(), metadata.uid(), metadata.gid()) };
    if chown_result != 0 {
        warnings.push(format!("ownership: {}", io::Error::last_os_error()));
    }

    let times = [
        libc::timespec { tv_sec: metadata.atime(), tv_nsec: metadata.atime_nsec() as _ },
        libc::timespec { tv_sec: metadata.mtime(), tv_nsec: metadata.mtime_nsec() as _ },
    ];
    let time_result = unsafe { libc::utimensat(libc::AT_FDCWD, destination.as_ptr(), times.as_ptr(), 0) };
    if time_result != 0 {
        warnings.push(format!("timestamps: {}", io::Error::last_os_error()));
    }

    #[cfg(target_os = "linux")]
    warnings.extend(preserve_linux_xattrs(src, dst));
    warnings
}

#[cfg(not(unix))]
fn preserve_unix_metadata(
    _src: &Path,
    _dst: &Path,
    _metadata: &fs::Metadata,
) -> Vec<String> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn preserve_linux_xattrs(src: &Path, dst: &Path) -> Vec<String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let mut warnings = Vec::new();
    let source = match CString::new(src.as_os_str().as_bytes()) {
        Ok(source) => source,
        Err(_) => return vec!["source path contains NUL while copying extended attributes".to_string()],
    };
    let destination = match CString::new(dst.as_os_str().as_bytes()) {
        Ok(destination) => destination,
        Err(_) => {
            return vec![
                "destination path contains NUL while copying extended attributes".to_string(),
            ]
        }
    };
    let size = unsafe { libc::listxattr(source.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EOPNOTSUPP) {
            return warnings;
        }
        warnings.push(format!("list extended attributes: {err}"));
        return warnings;
    }
    if size == 0 {
        return warnings;
    }
    let mut names = vec![0u8; size as usize];
    let read = unsafe { libc::listxattr(source.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    if read < 0 {
        warnings.push(format!(
            "read extended attribute names: {}",
            io::Error::last_os_error()
        ));
        return warnings;
    }
    for raw_name in names[..read as usize].split(|byte| *byte == 0).filter(|name| !name.is_empty()) {
        let name = match CString::new(raw_name) {
            Ok(name) => name,
            Err(_) => {
                warnings.push("extended attribute name contains NUL".to_string());
                continue;
            }
        };
        let value_size = unsafe { libc::getxattr(source.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if value_size < 0 {
            warnings.push(format!(
                "read extended attribute {:?}: {}",
                String::from_utf8_lossy(raw_name),
                io::Error::last_os_error()
            ));
            continue;
        }
        let mut value = vec![0u8; value_size as usize];
        if value_size > 0 {
            let got = unsafe { libc::getxattr(source.as_ptr(), name.as_ptr(), value.as_mut_ptr().cast(), value.len()) };
            if got < 0 {
                warnings.push(format!(
                    "read extended attribute {:?}: {}",
                    String::from_utf8_lossy(raw_name),
                    io::Error::last_os_error()
                ));
                continue;
            }
            value.truncate(got as usize);
        }
        let set = unsafe { libc::setxattr(destination.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0) };
        if set != 0 {
            warnings.push(format!(
                "write extended attribute {:?}: {}",
                String::from_utf8_lossy(raw_name),
                io::Error::last_os_error()
            ));
        }
    }
    warnings
}

fn delete_path(path: &Path, policy: DeletePolicy) -> Result<(), FilePickerError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| io_error("read delete metadata", path, err))?;
    if metadata.is_dir() {
        match policy {
            DeletePolicy::FilesAndEmptyDirectories => fs::remove_dir(path).map_err(|err| io_error("delete empty directory", path, err)),
            DeletePolicy::Recursive => fs::remove_dir_all(path).map_err(|err| io_error("delete directory recursively", path, err)),
        }
    } else {
        fs::remove_file(path).map_err(|err| io_error("delete file", path, err))
    }
}


#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

fn create_unique_staging_directory(parent: &Path) -> Result<PathBuf, FilePickerError> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let pid = std::process::id();
    for _ in 0..10_000usize {
        let index = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let candidate = parent.join(format!(".tui-file-picker-copying-{pid}-{index}"));
        match create_private_directory(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(io_error("create staging directory", &candidate, err)),
        }
    }
    Err(FilePickerError::Io {
        op: "create staging directory",
        path: parent.to_path_buf(),
        message: "exhausted unique staging names".to_string(),
    })
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn cleanup_staging(path: &Path) {
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

fn io_error(op: &'static str, path: &Path, err: io::Error) -> FilePickerError {
    FilePickerError::Io { op, path: path.to_path_buf(), message: err.to_string() }
}

fn is_cross_device_error(err: &io::Error) -> bool {
    #[cfg(unix)]
    {
        const EXDEV: i32 = 18;
        if err.raw_os_error() == Some(EXDEV) {
            return true;
        }
    }
    #[cfg(windows)]
    {
        const ERROR_NOT_SAME_DEVICE: i32 = 17;
        if err.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE) {
            return true;
        }
    }
    false
}

#[allow(dead_code)]
pub(crate) fn root_path() -> PathBuf {
    filesystem_root()
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn delete_io_errors_have_status_bar_copy_without_full_path() {
        let path = PathBuf::from("/tmp/example/front 2.jpg");
        let error = FilePickerError::Io {
            op: "delete file",
            path: path.clone(),
            message: "permission denied".to_string(),
        };

        assert_eq!(error.status_message(), "Delete failed: permission denied");
        assert!(error.message().contains(&path.display().to_string()));
    }

    #[test]
    fn navigation_maintains_back_forward_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let one = temp.path().join("one");
        let two = temp.path().join("two");
        fs::create_dir(&one).expect("one");
        fs::create_dir(&two).expect("two");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        assert!(picker.navigate_to_dir(one.clone()));
        assert!(picker.go_back());
        assert!(same_path(picker.current_dir(), temp.path()));
        assert!(picker.go_forward());
        assert!(same_path(picker.current_dir(), &one));
        assert!(picker.navigate_to_dir(two));
        assert!(picker.history_forward.is_empty());
    }

    #[test]
    fn image_filter_enables_preview_pane_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        assert!(picker.show_preview());
    }

    #[test]
    fn explicit_preview_config_enables_custom_preview_pane() {
        let temp = tempfile::tempdir().expect("tempdir");
        let picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            show_preview: true,
            ..FilePickerConfig::default()
        });
        assert!(picker.show_preview());
    }

    #[test]
    fn directories_are_visible_under_image_filter() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(temp.path().join("child")).expect("dir");
        fs::write(temp.path().join("cover.png"), b"img").expect("img");
        fs::write(temp.path().join("notes.txt"), b"txt").expect("txt");
        let picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        assert!(picker.entries().iter().any(|entry| entry.name == "child"));
        assert!(picker.entries().iter().any(|entry| entry.name == "cover.png"));
        assert!(!picker.entries().iter().any(|entry| entry.name == "notes.txt"));
    }

    #[test]
    fn directory_mode_accepts_current_dir_not_highlighted_child() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        fs::create_dir(&child).expect("child");

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Directories,
            ..FilePickerConfig::default()
        });
        let child_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == child)
            .expect("child visible");
        picker.set_file_cursor(child_index, 8);

        assert_eq!(picker.selected_path(), Some(child.as_path()));
        assert_eq!(
            picker.accept_current_selection(),
            FilePickerAction::Selected(temp.path().to_path_buf())
        );
        assert_eq!(picker.current_dir(), temp.path());
    }

    #[test]
    fn address_file_path_does_not_select_file_in_directory_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"png").expect("file");

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Directories,
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });

        picker.begin_address_edit();
        picker.address_input = TextInputState::new(file.display().to_string());

        assert_eq!(picker.commit_address(), FilePickerAction::None);
        assert!(matches!(
            picker.last_error(),
            Some(FilePickerError::WrongSelectionMode(_))
        ));
        assert_eq!(picker.focus(), FilePickerFocus::Address);
        assert!(same_path(picker.current_dir(), temp.path()));
    }

    #[test]
    fn address_file_path_selects_file_in_file_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"png").expect("file");

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Files,
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });

        picker.begin_address_edit();
        picker.address_input = TextInputState::new(file.display().to_string());

        assert_eq!(picker.commit_address(), FilePickerAction::Selected(file.clone()));
        assert!(same_path(picker.current_dir(), temp.path()));
        assert_eq!(picker.selected_path(), Some(file.as_path()));
    }


    #[test]
    fn request_delete_current_opens_confirmation_without_setting_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"png").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        let index = picker.entries().iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_file_cursor(index, 4);
        picker.set_error(FilePickerError::NoSelection);

        assert!(picker.request_delete_current());

        assert_eq!(picker.focus(), FilePickerFocus::DeleteConfirm);
        assert_eq!(picker.pending_delete, vec![file.clone()]);
        assert_eq!(picker.delete_confirm_button, DeleteConfirmButton::Cancel);
        assert!(picker.last_error().is_none(), "confirmation is expected UI state, not an error");
        assert!(file.exists(), "requesting confirmation must not delete immediately");
    }

    #[test]
    fn new_item_requires_prompted_name_before_creating() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        assert!(picker.create_new_file());
        assert_eq!(picker.focus(), FilePickerFocus::CreateName);
        assert!(!temp.path().join("untitled.txt").exists());
        picker.create_name_input = TextInputState::new("named.txt".to_string());
        assert!(picker.commit_create_name());
        assert!(temp.path().join("named.txt").exists());
    }



    #[cfg(feature = "image-preview")]
    #[test]
    fn image_preview_load_is_requested_on_image_selection_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("a.png");
        let second = temp.path().join("b.png");
        fs::write(&first, b"not a real png").expect("first");
        fs::write(&second, b"not a real png either").expect("second");

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        let second_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == second)
            .expect("second image visible");

        picker.set_file_cursor(second_index, 4);

        assert_eq!(picker.image_preview_cache.path.as_deref(), Some(second.as_path()));
        assert!(
            picker.image_preview_cache.receiver.is_some()
                || picker.image_preview_cache.decoded_image.is_some()
                || picker.image_preview_cache.error.is_some(),
            "selection change should create or complete an async preview request"
        );
    }

    #[cfg(feature = "image-preview")]
    #[test]
    fn image_preview_cache_invalidation_drops_async_and_decoded_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"not a real png").expect("file");

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        picker.request_image_preview_load(file.clone());
        assert_eq!(picker.image_preview_cache.path.as_deref(), Some(file.as_path()));

        picker.invalidate_image_preview_cache();

        assert!(picker.image_preview_cache.path.is_none());
        assert!(picker.image_preview_cache.receiver.is_none());
        assert!(picker.image_preview_cache.decoded_image.is_none());
        assert!(picker.image_preview_cache.protocol.is_none());
    }

    #[test]
    fn copy_never_overwrites_an_existing_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&source, b"source").expect("source");
        fs::write(&destination, b"destination").expect("destination");

        let error = safe_copy_path(&source, &destination, FileOperationPolicy::default())
            .expect_err("existing destination must be rejected");

        assert!(matches!(error, FilePickerError::DestinationExists(path) if path == destination));
        assert_eq!(fs::read(&source).expect("source remains"), b"source");
        assert_eq!(fs::read(&destination).expect("destination remains"), b"destination");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interactive_rename_never_replaces_a_concurrently_existing_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&source, b"source").expect("source");
        fs::write(&destination, b"destination").expect("destination");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.selected = Some(source.clone());

        let error = picker
            .try_rename_current("destination.txt")
            .expect_err("rename collision must be rejected");

        assert!(matches!(error, FilePickerError::DestinationExists(path) if path == destination));
        assert_eq!(fs::read(&source).expect("source remains"), b"source");
        assert_eq!(
            fs::read(temp.path().join("destination.txt")).expect("destination remains"),
            b"destination"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn committed_directory_rename_remaps_current_dir_without_revalidating_old_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let nested = source.join("nested");
        fs::create_dir_all(&nested).expect("nested source");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: nested.clone(),
            ..FilePickerConfig::default()
        });
        assert!(picker.begin_rename_path(source.clone()));
        picker.create_name_input = TextInputState::new("renamed".to_string());

        assert!(picker.commit_create_name());
        assert_eq!(picker.current_dir(), temp.path().join("renamed").join("nested"));
        assert!(!source.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn partial_cut_reports_committed_roots_and_only_retryable_sources() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        let first = source_dir.join("first.txt");
        let second = source_dir.join("second.txt");
        fs::write(&first, b"first").expect("first");
        fs::write(&second, b"second").expect("second");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![first.clone(), second.clone()],
        )
        .expect("clipboard");
        let plan = plan_filesystem_paste(&clipboard, &destination_dir).expect("plan");
        // Simulate a destination race after preflight but before the second root.
        fs::write(destination_dir.join("second.txt"), b"other actor").expect("racing destination");
        let mut progress = |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());

        let failure = execute_paste_plan_progress(
            &plan,
            FileOperationPolicy::default(),
            &mut progress,
        )
        .expect_err("second no-clobber move must fail after first commits");

        assert_eq!(failure.completed, vec![plan.mappings[0].clone()]);
        assert_eq!(failure.remaining_sources, vec![second.clone()]);
        assert!(!first.exists());
        assert_eq!(fs::read(destination_dir.join("first.txt")).expect("moved first"), b"first");
        assert_eq!(fs::read(&second).expect("retry source remains"), b"second");
        assert_eq!(
            fs::read(destination_dir.join("second.txt")).expect("racing destination preserved"),
            b"other actor"
        );
    }

    #[test]
    fn failed_back_navigation_preserves_both_history_stacks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let missing = temp.path().join("missing");
        picker.history_back.push(missing.clone());
        let current = picker.current_dir.clone();
        let forward = picker.history_forward.clone();

        assert!(!picker.go_back());
        assert_eq!(picker.current_dir, current);
        assert_eq!(picker.history_back, vec![missing]);
        assert_eq!(picker.history_forward, forward);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directory_is_not_classified_as_an_ordinary_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        fs::create_dir(&target).expect("target");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let entry = picker
            .entries()
            .iter()
            .find(|entry| entry.path == link)
            .expect("link entry");
        assert!(!entry.is_dir);
        assert_eq!(entry.file_type, "Symlink");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn atomic_no_replace_rename_preserves_both_paths_on_collision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&source, b"source").expect("source");
        fs::write(&destination, b"destination").expect("destination");

        match rename_no_replace(&source, &destination) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
            result => panic!("unexpected no-replace result: {result:?}"),
        }

        assert_eq!(fs::read(&source).expect("source remains"), b"source");
        assert_eq!(fs::read(&destination).expect("destination remains"), b"destination");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_copy_is_rejected_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let link = temp.path().join("link.txt");
        let dst = temp.path().join("copy.txt");
        fs::write(&source, b"ok").expect("source");
        make_symlink(&source, &link).expect("symlink");
        let err = safe_copy_path(&link, &dst, FileOperationPolicy::default()).expect_err("reject symlink");
        assert!(matches!(err, FilePickerError::SymlinkRejected(_)));
        assert!(!dst.exists());
    }

    #[cfg(unix)]
    fn make_symlink(src: &Path, dst: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(src, dst)
    }

    #[cfg(windows)]
    fn make_symlink(src: &Path, dst: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_file(src, dst)
    }
}
