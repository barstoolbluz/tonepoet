use crate::filter::FilePickerFilter;
use crate::theme::FilePickerTheme;
use crate::tree::{filesystem_root, initial_tree_nodes, refresh_tree_children};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(feature = "image-preview")]
use std::sync::mpsc::{self, Receiver};
#[cfg(feature = "image-preview")]
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
    Menu,
    Submenu,
    Properties,
    DeleteConfirm,
    CreateName,
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
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    Back,
    Forward,
    Up,
    FileOperations,
    Properties,
    Go,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerMenuAction {
    NewFile,
    NewFolder,
    Cut,
    Copy,
    Paste,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerHitAction {
    Toolbar(ToolbarAction),
    Address,
    TreeRow(usize),
    FileRow(usize),
    Menu(FilePickerMenuAction),
    MenuNew,
    Submenu(FilePickerMenuAction),
    PropertiesClose,
    DeleteConfirm,
    DeleteCancel,
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
    pub operation_policy: FileOperationPolicy,
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
            operation_policy: FileOperationPolicy::default(),
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

#[derive(Debug, Clone)]
pub struct FilePickerState {
    pub(crate) current_dir: PathBuf,
    pub(crate) history_back: Vec<PathBuf>,
    pub(crate) history_forward: Vec<PathBuf>,
    pub(crate) address_editing: bool,
    pub(crate) address_buffer: String,
    pub(crate) address_cursor: usize,
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
    pub(crate) selected: Option<PathBuf>,
    pub(crate) title: String,
    pub(crate) theme: FilePickerTheme,
    pub(crate) focus: FilePickerFocus,
    pub(crate) previous_focus: FilePickerFocus,
    pub(crate) selection_mode: FilePickerSelectionMode,
    pub(crate) show_hidden: bool,
    pub(crate) show_preview: bool,
    #[cfg(feature = "image-preview")]
    pub(crate) image_preview_cache: ImagePreviewCache,
    pub(crate) sort_key: FilePickerSortKey,
    pub(crate) sort_reverse: bool,
    pub(crate) clipboard: Option<FilePickerClipboard>,
    pub(crate) pending_delete: Option<PathBuf>,
    pub(crate) delete_confirm_button: DeleteConfirmButton,
    pub(crate) properties_open: bool,
    pub(crate) last_error: Option<FilePickerError>,
    pub(crate) hit_regions: Vec<HitRegion>,
    pub(crate) toolbar_button_geometry: Vec<ToolbarButtonGeometry>,
    pub(crate) last_layout: Option<FilePickerLayoutMetrics>,
    pub(crate) last_click: Option<LastClick>,
    pub(crate) double_click_window: Duration,
    pub(crate) free_space_bytes: Option<u64>,
    pub(crate) operation_policy: FileOperationPolicy,
    pub(crate) pending_create: Option<FilePickerCreateKind>,
    pub(crate) create_name_buffer: String,
    pub(crate) create_name_cursor: usize,
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
            address_buffer: start_dir.display().to_string(),
            address_cursor: start_dir.display().to_string().len(),
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
            selected: None,
            title: config.title,
            theme: config.theme,
            focus: FilePickerFocus::Files,
            previous_focus: FilePickerFocus::Files,
            selection_mode: config.selection_mode,
            show_hidden: config.show_hidden,
            show_preview,
            #[cfg(feature = "image-preview")]
            image_preview_cache: ImagePreviewCache::default(),
            sort_key: FilePickerSortKey::Name,
            sort_reverse: false,
            clipboard: None,
            pending_delete: None,
            delete_confirm_button: DeleteConfirmButton::Cancel,
            properties_open: false,
            last_error: None,
            hit_regions: Vec::new(),
            toolbar_button_geometry: Vec::new(),
            last_layout: None,
            last_click: None,
            double_click_window: Duration::from_millis(500),
            free_space_bytes: None,
            operation_policy: config.operation_policy,
            pending_create: None,
            create_name_buffer: String::new(),
            create_name_cursor: 0,
        };
        state.refresh();
        state.select_tree_node_for_current_dir();
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
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            if !self.show_hidden && name.starts_with('.') {
                continue;
            }
            let metadata = match item.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let is_dir = metadata.is_dir();
            if !self.filter.accepts_path(&path, is_dir) {
                continue;
            }
            entries.push(FilePickerEntry {
                name,
                path,
                is_dir,
                size: metadata.is_file().then_some(metadata.len()),
                file_type: file_type_label(item.path(), is_dir, metadata.file_type().is_symlink()),
                modified: metadata.modified().ok(),
            });
        }

        let sort_key = self.sort_key;
        let reverse = self.sort_reverse;
        entries.sort_by(|a, b| compare_entries(a, b, sort_key, reverse));
        self.entries = entries;
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
        self.pending_delete = None;
        self.delete_confirm_button = DeleteConfirmButton::Cancel;
        self.pending_create = None;
        self.focus = FilePickerFocus::Files;
        self.previous_focus = FilePickerFocus::Files;
        self.tree_focused = false;
        self.refresh();
        true
    }

    pub fn go_back(&mut self) -> bool {
        let Some(prior) = self.history_back.pop() else {
            return false;
        };
        self.history_forward.push(self.current_dir.clone());
        self.navigate_to_dir_with_history(prior, false)
    }

    pub fn go_forward(&mut self) -> bool {
        let Some(next) = self.history_forward.pop() else {
            return false;
        };
        self.history_back.push(self.current_dir.clone());
        self.navigate_to_dir_with_history(next, false)
    }

    pub fn go_parent(&mut self) -> bool {
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return false;
        };
        self.navigate_to_dir(parent)
    }

    pub fn commit_address(&mut self) -> FilePickerAction {
        let input = self.address_buffer.trim();
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
        self.address_buffer = self.current_dir.display().to_string();
        self.address_cursor = self.address_buffer.len();
    }

    pub fn begin_address_edit(&mut self) {
        self.address_editing = true;
        self.previous_focus = self.focus;
        self.focus = FilePickerFocus::Address;
        self.menu_open = false;
        self.submenu_open = false;
        self.sync_address_from_current_dir();
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
            if let Some(selected) = self.selected.clone().filter(|path| path.is_dir()) {
                return FilePickerAction::Selected(selected);
            }
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

    pub(crate) fn begin_create_name(&mut self, kind: FilePickerCreateKind) {
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
        self.create_name_buffer = match kind {
            FilePickerCreateKind::File => unique_path(&self.current_dir.join("untitled.txt"))
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("untitled.txt")
                .to_string(),
            FilePickerCreateKind::Folder => unique_path(&self.current_dir.join("New Folder"))
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("New Folder")
                .to_string(),
        };
        self.create_name_cursor = self.create_name_buffer.len();
        self.menu_open = false;
        self.submenu_open = false;
        self.previous_focus = self.focus;
        self.focus = FilePickerFocus::CreateName;
        self.clear_error();
    }

    pub(crate) fn cancel_create_name(&mut self) {
        self.pending_create = None;
        self.create_name_buffer.clear();
        self.create_name_cursor = 0;
        self.focus = FilePickerFocus::Files;
        self.clear_error();
    }

    pub(crate) fn commit_create_name(&mut self) -> bool {
        let Some(kind) = self.pending_create else {
            self.set_error(FilePickerError::InvalidNewItemName("no pending item type".to_string()));
            return false;
        };
        let name = self.create_name_buffer.trim().to_string();
        match self.try_create_named_item(kind, &name) {
            Ok(()) => {
                self.pending_create = None;
                self.create_name_buffer.clear();
                self.create_name_cursor = 0;
                self.focus = FilePickerFocus::Files;
                true
            }
            Err(err) => {
                self.set_error(err);
                false
            }
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
        let path = self.current_dir.join(name);
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
        self.selected = Some(path.clone());
        self.refresh();
        self.select_path_in_entries(&path);
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
        let entry = self.entries.get(self.file_cursor).cloned().ok_or(FilePickerError::NoSelection)?;
        self.clipboard = Some(FilePickerClipboard { mode: FilePickerClipboardMode::Cut, path: entry.path });
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
        let entry = self.entries.get(self.file_cursor).cloned().ok_or(FilePickerError::NoSelection)?;
        self.clipboard = Some(FilePickerClipboard { mode: FilePickerClipboardMode::Copy, path: entry.path });
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
        if !self.operation_policy.allow_paste {
            return Err(FilePickerError::OperationDisabled("paste"));
        }
        let clipboard = self.clipboard.clone().ok_or(FilePickerError::ClipboardEmpty)?;
        if !clipboard.path.exists() {
            return Err(FilePickerError::ClipboardSourceMissing(clipboard.path));
        }
        let name = clipboard
            .path
            .file_name()
            .ok_or_else(|| FilePickerError::ClipboardPathHasNoFileName(clipboard.path.clone()))?;
        let destination = unique_path(&self.current_dir.join(name));
        match clipboard.mode {
            FilePickerClipboardMode::Cut => self.move_path(&clipboard.path, &destination)?,
            FilePickerClipboardMode::Copy => safe_copy_path(&clipboard.path, &destination, self.operation_policy)?,
        }
        if clipboard.mode == FilePickerClipboardMode::Cut {
            self.clipboard = None;
        }
        self.selected = Some(destination.clone());
        self.refresh();
        self.select_path_in_entries(&destination);
        Ok(())
    }

    fn move_path(&mut self, source: &Path, destination: &Path) -> Result<(), FilePickerError> {
        match fs::rename(source, destination) {
            Ok(()) => Ok(()),
            Err(err) if is_cross_device_error(&err) => match self.operation_policy.cross_device_cut {
                CrossDeviceCutPolicy::Reject => Err(FilePickerError::CrossDeviceMoveRejected {
                    source: source.to_path_buf(),
                    destination: destination.to_path_buf(),
                }),
                CrossDeviceCutPolicy::CopyThenDelete => {
                    safe_copy_path(source, destination, self.operation_policy)?;
                    match delete_path(source, self.operation_policy.delete) {
                        Ok(()) => Ok(()),
                        Err(err) => {
                            cleanup_staging(destination);
                            Err(err)
                        }
                    }
                }
            },
            Err(err) => Err(io_error("move", source, err)),
        }
    }

    pub fn request_delete_current(&mut self) -> bool {
        if !self.operation_policy.allow_delete {
            self.set_error(FilePickerError::OperationDisabled("delete"));
            return false;
        }
        match self.entries.get(self.file_cursor).cloned() {
            Some(entry) => {
                self.pending_delete = Some(entry.path.clone());
                self.previous_focus = self.focus;
                self.focus = FilePickerFocus::DeleteConfirm;
                self.delete_confirm_button = DeleteConfirmButton::Cancel;
                self.clear_error();
                true
            }
            None => {
                self.set_error(FilePickerError::NoSelection);
                false
            }
        }
    }

    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
        self.delete_confirm_button = DeleteConfirmButton::Cancel;
        self.focus = FilePickerFocus::Files;
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
        let path = self.pending_delete.clone().ok_or(FilePickerError::NoPendingDelete)?;
        delete_path(&path, self.operation_policy.delete)?;
        if self.selected.as_ref() == Some(&path) {
            self.selected = None;
        }
        self.pending_delete = None;
        self.delete_confirm_button = DeleteConfirmButton::Cancel;
        self.focus = FilePickerFocus::Files;
        self.refresh();
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

    pub(crate) fn is_new_menu_enabled(&self) -> bool {
        self.operation_policy.allow_new_file || self.operation_policy.allow_new_folder
    }

    pub(crate) fn is_menu_action_enabled(&self, action: FilePickerMenuAction) -> bool {
        match action {
            FilePickerMenuAction::NewFile => self.operation_policy.allow_new_file,
            FilePickerMenuAction::NewFolder => self.operation_policy.allow_new_folder,
            FilePickerMenuAction::Cut => self.operation_policy.allow_cut && self.current_selection().is_some(),
            FilePickerMenuAction::Copy => self.operation_policy.allow_copy && self.current_selection().is_some(),
            FilePickerMenuAction::Delete => self.operation_policy.allow_delete && self.current_selection().is_some(),
            FilePickerMenuAction::Paste => {
                self.operation_policy.allow_paste
                    && self.clipboard.as_ref().map(|clipboard| clipboard.path.exists()).unwrap_or(false)
            }
        }
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
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err(FilePickerError::InvalidNewItemName(name.to_string()));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || Path::new(trimmed).components().count() != 1 {
        return Err(FilePickerError::InvalidNewItemName(name.to_string()));
    }
    Ok(())
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

fn safe_copy_path(src: &Path, dst: &Path, policy: FileOperationPolicy) -> Result<(), FilePickerError> {
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    if dst.exists() {
        return Err(FilePickerError::DestinationExists(dst.to_path_buf()));
    }
    let staging = unique_staging_path(parent, dst.file_name().and_then(OsStr::to_str).unwrap_or("item"));
    let mut visited = HashSet::new();
    let result = copy_path_to_staging(src, &staging, policy, &mut visited)
        .and_then(|()| fs::rename(&staging, dst).map_err(|err| io_error("commit staged copy", dst, err)));
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            cleanup_staging(&staging);
            Err(err)
        }
    }
}

fn copy_path_to_staging(
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), FilePickerError> {
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
        let result = copy_dir_contents(src, dst, policy, visited);
        visited.remove(&src_canon);
        result
    } else if metadata.is_file() {
        fs::copy(src, dst).map(|_| ()).map_err(|err| io_error("copy file", src, err))
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
) -> Result<(), FilePickerError> {
    for entry in fs::read_dir(src).map_err(|err| io_error("read directory", src, err))? {
        let entry = entry.map_err(|err| io_error("read directory entry", src, err))?;
        let child_src = entry.path();
        let child_dst = dst.join(entry.file_name());
        copy_path_to_staging(&child_src, &child_dst, policy, visited)?;
    }
    Ok(())
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

fn unique_staging_path(parent: &Path, name: &str) -> PathBuf {
    let pid = std::process::id();
    for index in 0..10_000usize {
        let candidate = parent.join(format!(".tui-file-picker-copying-{pid}-{index}-{name}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!(".tui-file-picker-copying-{pid}-{name}"))
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
        picker.address_buffer = file.display().to_string();
        picker.address_cursor = picker.address_buffer.len();

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
        picker.address_buffer = file.display().to_string();
        picker.address_cursor = picker.address_buffer.len();

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
        assert_eq!(picker.pending_delete.as_deref(), Some(file.as_path()));
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
        picker.create_name_buffer = "named.txt".to_string();
        picker.create_name_cursor = picker.create_name_buffer.len();
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
