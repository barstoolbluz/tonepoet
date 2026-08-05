use crate::filter::FilePickerFilter;
use crate::bookmarks::{BookmarkNameAction, FilePickerBookmarks};
use crate::filesystem_clipboard::FilesystemClipboard;
use crate::search::FileSearchState;
use crate::type_ahead::TypeAheadState;
use crate::theme::FilePickerTheme;
use crate::text_input::TextInputState;
use crate::tree::{filesystem_root, refresh_tree_children};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering as AtomicOrdering};
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
    /// Explicit confirm of the marked files in current visible order.
    /// Directory marks are deliberately excluded at the picker boundary.
    SelectedMany(Vec<PathBuf>),
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
    SortName,
    SortSize,
    SortType,
    SortModified,
    TextCut,
    TextCopy,
    TextPaste,
    TextDelete,
    TextSelectAll,
    TextTitleCase,
    TextUppercase,
    TextLowercase,
    RenameTitleCase,
    RenameUppercase,
    RenameLowercase,
    OpenSystemDefault,
    AddBookmark,
    OpenBookmarks,
}

/// The surface that owns an open file-picker context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePickerContextMenuKind {
    Toolbar,
    Address,
    NameEditor,
    SaveNameEditor,
    SearchEditor,
    BookmarkNameEditor,
    Tree,
    File,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePickerSubmenuKind {
    New,
    Selection,
    Sort,
    Rename,
    TextCase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePickerMenuEntry {
    NewSubmenu,
    SelectionSubmenu,
    SortSubmenu,
    RenameSubmenu,
    CaseSubmenu,
    Action(FilePickerMenuAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePickerSubmenuEntry {
    CaseSubmenu,
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
    SortColumn(FilePickerSortKey),
    CreateNameEditor,
    SaveNameEditor,
    SearchInput,
    BookmarkNameEditor,
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
    MenuSort,
    MenuRename,
    MenuCase,
    SubmenuCase,
    Submenu(FilePickerMenuAction),
    NestedSubmenu(FilePickerMenuAction),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisualRangeSelection {
    pub anchor: PathBuf,
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
    /// Copy to the destination with staging, verify it, then recursively
    /// remove the quarantined source tree as completion of the authorized move.
    /// The separate explicit-delete policy does not limit move cleanup.
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

/// Verification depth for file operations and their retained undo proofs.
///
/// `Standard` is intentionally identity- and metadata-based: it keeps
/// no-clobber, bounded tree-membership, size/version, and stale-object checks
/// without adding content reads after the bytes have already been copied.
/// `Strong` preserves the historical full-content proof machinery.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum VerificationMode {
    Standard,
    Strong,
}

impl Default for VerificationMode {
    fn default() -> Self {
        Self::Standard
    }
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
    /// Policy for explicit permanent deletion commands. Recursive deletion is
    /// opt-in. This does not restrict cleanup of a copied-and-verified move.
    pub delete: DeletePolicy,
    /// Include routine reduced-filesystem capability notices in successful
    /// operation results. Failures and data-affecting warnings remain visible.
    pub verbose_degrade_notices: bool,
    /// Select identity-level default verification or the historical full
    /// content-authority path.
    pub verification: VerificationMode,
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
            verbose_degrade_notices: false,
            verification: VerificationMode::Standard,
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
    OperationCommittedButUnverified {
        source: PathBuf,
        destination: PathBuf,
        message: String,
    },
    DestinationCommittedMoveIncomplete {
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
            Self::OperationCommittedButUnverified { source, destination, message } => format!(
                "Operation failed after publishing {} from {} because the destination could not be verified: {}",
                destination.display(),
                source.display(),
                message
            ),
            Self::DestinationCommittedMoveIncomplete { source, destination, message } => format!(
                "Move incomplete after publishing {} from {}: {}",
                destination.display(),
                source.display(),
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

fn default_picker_title_case(value: &str) -> String {
    const SMALL_WORDS: &[&str] = &[
        "a", "an", "the", "and", "but", "or", "for", "nor", "on", "at", "to",
        "from", "by", "of", "as", "about", "in", "up", "with",
    ];

    fn affixes(word: &str) -> (&str, &str, &str) {
        let Some(start) = word
            .char_indices()
            .find_map(|(index, ch)| ch.is_alphanumeric().then_some(index))
        else {
            return (word, "", "");
        };
        let end = word
            .char_indices()
            .rev()
            .find_map(|(index, ch)| ch.is_alphanumeric().then_some(index + ch.len_utf8()))
            .unwrap_or(start);
        (&word[..start], &word[start..end], &word[end..])
    }

    fn special(core: &str) -> Option<&'static str> {
        match core.to_lowercase().as_str() {
            "ac/dc" | "acdc" | "ac-dc" => Some("AC/DC"),
            "esg" => Some("ESG"),
            "rem" | "r.e.m." => Some("R.E.M."),
            "csny" => Some("CSNY"),
            "elo" => Some("ELO"),
            "abba" => Some("ABBA"),
            "inxs" => Some("INXS"),
            "nwa" | "n.w.a" => Some("N.W.A"),
            "omg" => Some("OMG"),
            "uk" => Some("UK"),
            "usa" => Some("USA"),
            "ussr" => Some("USSR"),
            "nyc" => Some("NYC"),
            "la" => Some("LA"),
            "dj" => Some("DJ"),
            "mc" => Some("MC"),
            "tv" => Some("TV"),
            "mtv" => Some("MTV"),
            "bbc" => Some("BBC"),
            "zz" => Some("ZZ"),
            "xrcd" => Some("XRCD"),
            "xrcd2" => Some("XRCD2"),
            "xrcd24" => Some("XRCD24"),
            "jp" => Some("JP"),
            "lp" => Some("LP"),
            "ii" => Some("II"),
            "iii" => Some("III"),
            "iv" => Some("IV"),
            "v" => Some("V"),
            "vi" => Some("VI"),
            "vii" => Some("VII"),
            "viii" => Some("VIII"),
            "ix" => Some("IX"),
            "x" => Some("X"),
            "xi" => Some("XI"),
            "xii" => Some("XII"),
            "xiii" => Some("XIII"),
            "xiv" => Some("XIV"),
            "xv" => Some("XV"),
            _ => None,
        }
    }

    fn capitalize_core(core: &str) -> String {
        if let Some(index) = core.find('&') {
            let (left, right) = core.split_at(index);
            return format!(
                "{}&{}",
                capitalize_core(left),
                capitalize_core(&right[1..])
            );
        }
        if let Some(value) = special(core) {
            return value.to_string();
        }
        let character_count = core.chars().count();
        if (2..=5).contains(&character_count)
            && core
                .chars()
                .all(|ch| ch.is_uppercase() || !ch.is_alphabetic())
        {
            return core.to_string();
        }
        if let Some(index) = core.find('\'') {
            let (left, right) = core.split_at(index);
            return format!("{}{}", capitalize_core(left), right);
        }
        if let Some(index) = core.find('-') {
            let (left, right) = core.split_at(index);
            return format!("{}-{}", capitalize_core(left), capitalize_core(&right[1..]));
        }
        if let Some(index) = core.find('/') {
            let (left, right) = core.split_at(index);
            return format!("{}/{}", capitalize_core(left), capitalize_core(&right[1..]));
        }
        let mut chars = core.chars();
        match chars.next() {
            Some(first) => first
                .to_uppercase()
                .chain(chars.as_str().to_lowercase().chars())
                .collect(),
            None => String::new(),
        }
    }

    let mut ranges = Vec::new();
    let mut start = None;
    for (index, ch) in value.char_indices() {
        if ch.is_whitespace() {
            if let Some(begin) = start.take() {
                ranges.push(begin..index);
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        ranges.push(begin..value.len());
    }

    let normalize_all_caps = value.len() > 1
        && value
            .chars()
            .all(|ch| !ch.is_alphabetic() || ch.is_uppercase());
    let mut output = String::with_capacity(value.len());
    let mut copied = 0;
    for (index, range) in ranges.iter().enumerate() {
        output.push_str(&value[copied..range.start]);
        let original_word = &value[range.clone()];
        let (original_prefix, original_core, original_suffix) = affixes(original_word);
        let normalized_word;
        let word = if normalize_all_caps
            && !(original_prefix
                .chars()
                .any(|ch| matches!(ch, '(' | '[' | '{' | '"' | '\'' | '“' | '‘'))
                && original_core == "US")
        {
            normalized_word = format!(
                "{original_prefix}{}{original_suffix}",
                original_core.to_lowercase(),
            );
            normalized_word.as_str()
        } else {
            original_word
        };
        let (prefix, core, suffix) = affixes(word);
        if core.is_empty() {
            output.push_str(word);
            copied = range.end;
            continue;
        }
        let after_ampersand = (index > 0
            && value[ranges[index - 1].clone()].contains('&'))
            || prefix.contains('&');
        let starts_section = prefix
            .chars()
            .any(|ch| matches!(ch, '(' | '[' | '{' | '"' | '\'' | '“' | '‘'));
        let lower = core.to_lowercase();
        let transformed = if index == 0
            || index + 1 == ranges.len()
            || after_ampersand
            || starts_section
            || !SMALL_WORDS.contains(&lower.as_str())
        {
            capitalize_core(core)
        } else {
            lower
        };
        output.push_str(prefix);
        output.push_str(&transformed);
        output.push_str(suffix);
        copied = range.end;
    }
    output.push_str(&value[copied..]);
    output
}

#[derive(Debug, Clone)]
pub struct FilePickerConfig {
    pub start_dir: PathBuf,
    pub filter: FilePickerFilter,
    pub title: String,
    pub theme: FilePickerTheme,
    pub selection_mode: FilePickerSelectionMode,
    pub show_hidden: bool,
    /// Initial sort field. Hosts may persist this alongside their browsing
    /// preferences and pass it back when opening the next picker.
    pub sort_key: FilePickerSortKey,
    /// Initial direction for `sort_key`; false is ascending, true descending.
    pub sort_reverse: bool,
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
    /// Host-provided title-case policy used by editor and filename case menus.
    /// The default is a Unicode-preserving standalone implementation; hosts
    /// with an established naming policy should inject that exact function.
    pub title_case: fn(&str) -> String,
}

/// Equality deliberately ignores `title_case`: comparing function pointers is
/// unpredictable across codegen units, and the host-injected policy is not
/// part of the picker's observable configuration state.
impl PartialEq for FilePickerConfig {
    fn eq(&self, other: &Self) -> bool {
        self.start_dir == other.start_dir
            && self.filter == other.filter
            && self.title == other.title
            && self.theme == other.theme
            && self.selection_mode == other.selection_mode
            && self.show_hidden == other.show_hidden
            && self.sort_key == other.sort_key
            && self.sort_reverse == other.sort_reverse
            && self.show_preview == other.show_preview
            && self.conflict_policy == other.conflict_policy
            && self.operation_policy == other.operation_policy
            && self.hide_extension == other.hide_extension
            && self.save_mode == other.save_mode
    }
}

impl Eq for FilePickerConfig {}

impl Default for FilePickerConfig {
    fn default() -> Self {
        Self {
            start_dir: home_dir().unwrap_or_else(|| PathBuf::from(".")),
            filter: FilePickerFilter::All,
            title: "Select file".to_string(),
            theme: FilePickerTheme::default(),
            selection_mode: FilePickerSelectionMode::Files,
            show_hidden: false,
            sort_key: FilePickerSortKey::Name,
            sort_reverse: false,
            show_preview: false,
            conflict_policy: None,
            operation_policy: FileOperationPolicy::default(),
            hide_extension: None,
            save_mode: None,
            title_case: default_picker_title_case,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerTextTarget {
    Address,
    CreateName,
    SaveName,
    Search,
    BookmarkName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextPointerSession {
    pub target: PickerTextTarget,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LastTextClick {
    pub target: PickerTextTarget,
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
    plan: PastePlan,
    retry_plan: Option<PasteRetryPlan>,
}

impl fmt::Debug for PickerPasteTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PickerPasteTask")
            .field("progress", &self.progress)
            .field("has_receiver", &self.receiver.is_some())
            .field("clipboard", &self.clipboard)
            .field("target_dir", &self.target_dir)
            .field("plan", &self.plan)
            .field("retry_plan", &self.retry_plan)
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
            plan: self.plan.clone(),
            retry_plan: self.retry_plan.clone(),
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
    /// Exact visible-entry lookup rebuilt with each refresh. Range highlighting
    /// is therefore O(1) per rendered row and performs no filesystem I/O.
    pub(crate) visible_path_indices: HashMap<PathBuf, usize>,
    pub(crate) file_cursor: usize,
    pub(crate) file_scroll: usize,
    pub(crate) file_table_state: TableState,
    pub(crate) filter: FilePickerFilter,
    pub(crate) menu_open: bool,
    pub(crate) menu_cursor: usize,
    pub(crate) submenu_open: bool,
    pub(crate) submenu_cursor: usize,
    pub(crate) submenu_kind: FilePickerSubmenuKind,
    pub(crate) case_submenu_open: bool,
    pub(crate) case_submenu_cursor: usize,
    pub(crate) context_menu_kind: FilePickerContextMenuKind,
    pub(crate) context_menu_target: Option<PathBuf>,
    pub(crate) context_menu_anchor: Option<(u16, u16)>,
    pub(crate) selected: Option<PathBuf>,
    pub(crate) multi_selected: Vec<PathBuf>,
    /// Hash-backed membership for `multi_selected`. The vector remains only as
    /// a compact compatibility/order cache; all membership and range insertion
    /// decisions use this set. Deterministic emission is derived from the
    /// visible-entry order, never the vector order.
    pub(crate) multi_selected_lookup: HashSet<PathBuf>,
    /// Stable origin for additive range gestures. This is deliberately
    /// independent of pointer/double-click state because every key event
    /// clears the latter.
    pub(crate) range_anchor: Option<PathBuf>,
    /// Pending keyboard visual range. The persistent mark set is not mutated
    /// until the user commits with `v`, Space, or explicit confirmation.
    pub(crate) visual_range: Option<VisualRangeSelection>,
    /// Directory marks discarded by the most recent explicit confirmation.
    /// Hosts can surface this without widening `SelectedMany`.
    pub(crate) last_selection_ignored_directories: usize,
    /// Marks pruned because a refresh/filter/hidden change made them
    /// invisible. Kept as a one-shot disclosure so no selection disappears
    /// silently.
    pub(crate) last_selection_dropped_invisible: usize,
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
    pub(crate) sort_changed: bool,
    pub(crate) clipboard: Option<FilesystemClipboard>,
    /// One-shot request raised by Ctrl+Shift+V in a focused text editor.
    /// The embedding application owns the asynchronous host clipboard read.
    pub(crate) host_clipboard_paste_requested: bool,
    pub(crate) paste_task: Option<PickerPasteTask>,
    /// Exact source-to-destination mappings retained after an incomplete cut.
    /// This prevents retries from allocating a suffixed duplicate path.
    paste_retry_plan: Option<PasteRetryPlan>,
    pub(crate) pending_delete: Vec<PathBuf>,
    pub(crate) delete_confirm_button: DeleteConfirmButton,
    pub(crate) properties_open: bool,
    pub(crate) last_error: Option<FilePickerError>,
    pub(crate) hit_regions: Vec<HitRegion>,
    pub(crate) toolbar_button_geometry: Vec<ToolbarButtonGeometry>,
    pub(crate) last_layout: Option<FilePickerLayoutMetrics>,
    pub(crate) last_click: Option<LastClick>,
    pub(crate) tree_last_click: Option<(PathBuf, Instant)>,
    pub(crate) text_pointer: Option<TextPointerSession>,
    pub(crate) text_last_click: Option<LastTextClick>,
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
    pub(crate) title_case: fn(&str) -> String,
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
            tree_nodes: crate::tree::initial_tree_nodes_with_hidden(&start_dir, config.show_hidden),
            tree_cursor: 0,
            tree_scroll: 0,
            tree_focused: false,
            entries: Vec::new(),
            visible_path_indices: HashMap::new(),
            file_cursor: 0,
            file_scroll: 0,
            file_table_state: TableState::default(),
            filter: config.filter,
            menu_open: false,
            menu_cursor: 0,
            submenu_open: false,
            submenu_cursor: 0,
            submenu_kind: FilePickerSubmenuKind::New,
            case_submenu_open: false,
            case_submenu_cursor: 0,
            context_menu_kind: FilePickerContextMenuKind::Toolbar,
            context_menu_target: None,
            context_menu_anchor: None,
            selected: None,
            multi_selected: Vec::new(),
            multi_selected_lookup: HashSet::new(),
            range_anchor: None,
            visual_range: None,
            last_selection_ignored_directories: 0,
            last_selection_dropped_invisible: 0,
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
            sort_key: config.sort_key,
            sort_reverse: config.sort_reverse,
            sort_changed: false,
            clipboard: None,
            host_clipboard_paste_requested: false,
            paste_task: None,
            paste_retry_plan: None,
            pending_delete: Vec::new(),
            delete_confirm_button: DeleteConfirmButton::Cancel,
            properties_open: false,
            last_error: None,
            hit_regions: Vec::new(),
            toolbar_button_geometry: Vec::new(),
            last_layout: None,
            last_click: None,
            tree_last_click: None,
            text_pointer: None,
            text_last_click: None,
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
            title_case: config.title_case,
        };
        state.refresh();
        state.select_tree_node_for_current_dir();
        if state.save_mode.is_some() {
            state.focus = FilePickerFocus::SaveName;
        }
        state
    }

    /// Consume a host-clipboard paste request raised by the focused picker
    /// text editor. This keeps platform integration in the embedding app
    /// without widening the picker's terminal-action enum.
    pub fn take_host_clipboard_paste_request(&mut self) -> bool {
        std::mem::take(&mut self.host_clipboard_paste_requested)
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

    /// Whether a filesystem cut/copy payload is available for paste.
    pub fn has_filesystem_clipboard(&self) -> bool {
        self.clipboard.is_some()
    }

    /// Paths currently marked in the files pane. Tree rows never participate.
    pub fn multi_selected_paths(&self) -> &[PathBuf] {
        &self.multi_selected
    }

    pub fn is_path_multi_selected(&self, path: &Path) -> bool {
        self.multi_selected_lookup.contains(path) || self.is_path_in_visual_range(path)
    }

    pub(crate) fn replace_multi_selected<I>(&mut self, paths: I)
    where
        I: IntoIterator<Item = PathBuf>,
    {
        self.multi_selected.clear();
        self.multi_selected_lookup.clear();
        for path in paths {
            if self.multi_selected_lookup.insert(path.clone()) {
                self.multi_selected.push(path);
            }
        }
    }

    fn mark_path(&mut self, path: PathBuf) -> bool {
        if !self.multi_selected_lookup.insert(path.clone()) {
            return false;
        }
        self.multi_selected.push(path);
        true
    }

    fn unmark_path(&mut self, path: &Path) -> bool {
        if !self.multi_selected_lookup.remove(path) {
            return false;
        }
        self.multi_selected.retain(|candidate| candidate != path);
        true
    }

    fn clear_multi_selected(&mut self) {
        self.multi_selected.clear();
        self.multi_selected_lookup.clear();
    }

    fn retain_multi_selected(&mut self, mut keep: impl FnMut(&Path) -> bool) {
        self.multi_selected.retain(|path| keep(path));
        self.multi_selected_lookup = self.multi_selected.iter().cloned().collect();
        debug_assert_eq!(self.multi_selected_lookup.len(), self.multi_selected.len());
    }

    fn is_path_in_visual_range(&self, path: &Path) -> bool {
        let Some(visual) = self.visual_range.as_ref() else {
            return false;
        };
        let (Some(anchor_index), Some(path_index)) =
            (self.visible_index_of(&visual.anchor), self.visible_index_of(path))
        else {
            return false;
        };
        let (start, end) = if anchor_index <= self.file_cursor {
            (anchor_index, self.file_cursor)
        } else {
            (self.file_cursor, anchor_index)
        };
        (start..=end).contains(&path_index)
    }

    pub(crate) fn effective_selected_count(&self) -> usize {
        let Some(visual) = self.visual_range.as_ref() else {
            return self.multi_selected_lookup.len();
        };
        let Some(anchor_index) = self.visible_index_of(&visual.anchor) else {
            return self.multi_selected_lookup.len();
        };
        let endpoint_index = self.file_cursor.min(self.entries.len().saturating_sub(1));
        let (start, end) = if anchor_index <= endpoint_index {
            (anchor_index, endpoint_index)
        } else {
            (endpoint_index, anchor_index)
        };
        self.multi_selected_lookup.len()
            + self.entries[start..=end]
                .iter()
                .filter(|entry| !self.multi_selected_lookup.contains(&entry.path))
                .count()
    }

    fn visible_index_of(&self, path: &Path) -> Option<usize> {
        self.visible_path_indices.get(path).copied()
    }

    fn range_paths_between(&self, anchor: &Path, endpoint_index: usize) -> Vec<PathBuf> {
        let Some(anchor_index) = self.visible_index_of(anchor) else {
            return Vec::new();
        };
        let endpoint_index = endpoint_index.min(self.entries.len().saturating_sub(1));
        let (start, end) = if anchor_index <= endpoint_index {
            (anchor_index, endpoint_index)
        } else {
            (endpoint_index, anchor_index)
        };
        self.entries[start..=end]
            .iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub(crate) fn visual_range_paths(&self) -> Vec<PathBuf> {
        let Some(visual) = self.visual_range.as_ref() else {
            return Vec::new();
        };
        self.range_paths_between(&visual.anchor, self.file_cursor)
    }

    pub(crate) fn begin_or_commit_visual_range(&mut self) -> bool {
        if self.visual_range.is_some() {
            self.commit_visual_range();
            return true;
        }
        let Some(anchor) = self.current_selection().map(|entry| entry.path.clone()) else {
            return false;
        };
        self.range_anchor = Some(anchor.clone());
        self.visual_range = Some(VisualRangeSelection { anchor });
        true
    }

    pub(crate) fn commit_visual_range(&mut self) -> bool {
        if self.visual_range.is_none() {
            return false;
        }
        let paths = self.visual_range_paths();
        for path in paths {
            self.mark_path(path);
        }
        self.visual_range = None;
        true
    }

    pub(crate) fn cancel_visual_range(&mut self) -> bool {
        let cancelled = self.visual_range.take().is_some();
        if cancelled {
            self.range_anchor = None;
        }
        cancelled
    }

    pub(crate) fn mark_range_to_index(&mut self, endpoint_index: usize) -> bool {
        let fallback_anchor = self.current_selection().map(|entry| entry.path.clone());
        let Some(anchor) = self.range_anchor.clone().or(fallback_anchor) else {
            return false;
        };
        if self.range_anchor.is_none() {
            self.range_anchor = Some(anchor.clone());
        }
        let paths = self.range_paths_between(&anchor, endpoint_index);
        if paths.is_empty() {
            return false;
        }
        for path in paths {
            self.mark_path(path);
        }
        true
    }

    pub(crate) fn extend_range_with_cursor_move(
        &mut self,
        delta: isize,
        visible_rows: usize,
    ) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        if self.range_anchor.is_none() {
            self.range_anchor = self.current_selection().map(|entry| entry.path.clone());
        }
        self.move_file_cursor(delta, visible_rows);
        self.mark_range_to_index(self.file_cursor)
    }

    /// Number of directory marks filtered by the most recent explicit
    /// selection confirmation. The value is reset at the start of every
    /// confirmation attempt.
    pub fn last_selection_ignored_directories(&self) -> usize {
        self.last_selection_ignored_directories
    }

    /// Consume the directory-filter count associated with the most recent
    /// explicit confirmation. Hosts use this one-shot accessor so a later
    /// Enter, double-click, or cancellation cannot inherit stale context.
    pub fn take_last_selection_ignored_directories(&mut self) -> usize {
        std::mem::take(&mut self.last_selection_ignored_directories)
    }

    pub fn last_selection_dropped_invisible(&self) -> usize {
        self.last_selection_dropped_invisible
    }

    pub fn take_last_selection_dropped_invisible(&mut self) -> usize {
        std::mem::take(&mut self.last_selection_dropped_invisible)
    }

    fn marked_visible_counts_with(
        &self,
        mut is_marked: impl FnMut(&Path) -> bool,
    ) -> (usize, usize) {
        let mut files = 0usize;
        let mut directories = 0usize;
        for entry in &self.entries {
            if !is_marked(&entry.path) {
                continue;
            }
            if entry.is_dir {
                directories = directories.saturating_add(1);
            } else {
                files = files.saturating_add(1);
            }
        }
        (files, directories)
    }

    fn marked_visible_counts(&self) -> (usize, usize) {
        self.marked_visible_counts_with(|path| self.multi_selected_lookup.contains(path))
    }

    /// Return marked files in the same sorted order as the visible file pane,
    /// plus the number of marked directories that were intentionally ignored.
    /// This makes completion deterministic regardless of marking gesture. Path
    /// allocation is intentionally confined to explicit confirmation; render-
    /// time labels use `marked_visible_counts` and allocate no selected paths.
    pub(crate) fn marked_files_in_visible_order(&self) -> (Vec<PathBuf>, usize) {
        let (file_count, ignored_directories) = self.marked_visible_counts();
        let mut files = Vec::with_capacity(file_count);
        for entry in &self.entries {
            if !entry.is_dir && self.multi_selected_lookup.contains(&entry.path) {
                files.push(entry.path.clone());
            }
        }
        (files, ignored_directories)
    }

    /// Context-sensitive label for the explicit picker confirmation button.
    pub(crate) fn selection_confirmation_label(&self) -> Option<String> {
        if self.selection_mode == FilePickerSelectionMode::Directories {
            return Some("Select Folder".to_string());
        }
        let (marked_file_count, _) = self.marked_visible_counts();
        if marked_file_count > 0 {
            return Some(format!(
                "Select {} File{}",
                marked_file_count,
                if marked_file_count == 1 { "" } else { "s" }
            ));
        }
        let entry = self.current_selection()?;
        if entry.is_dir {
            (self.selection_mode == FilePickerSelectionMode::FilesOrDirectories)
                .then(|| "Select Folder".to_string())
        } else {
            Some("Select File".to_string())
        }
    }

    pub fn toggle_current_multi_selection(&mut self) -> bool {
        let Some(path) = self.current_selection().map(|entry| entry.path.clone()) else {
            return false;
        };
        self.visual_range = None;
        self.range_anchor = Some(path.clone());
        if !self.unmark_path(&path) {
            self.mark_path(path);
        }
        true
    }

    pub(crate) fn toggle_current_multi_selection_and_advance(
        &mut self,
        visible_rows: usize,
    ) -> bool {
        if !self.toggle_current_multi_selection() {
            return false;
        }
        if self.file_cursor + 1 < self.entries.len() {
            self.move_file_cursor(1, visible_rows);
        }
        true
    }

    pub fn select_all_visible(&mut self) {
        self.visual_range = None;
        self.range_anchor = None;
        let paths = self.entries.iter().map(|entry| entry.path.clone()).collect::<Vec<_>>();
        self.replace_multi_selected(paths);
    }

    pub fn invert_visible_selection(&mut self) {
        self.visual_range = None;
        self.range_anchor = None;
        let selected = std::mem::take(&mut self.multi_selected_lookup);
        self.multi_selected.clear();
        let paths = self
            .entries
            .iter()
            .filter(|entry| !selected.contains(&entry.path))
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        self.replace_multi_selected(paths);
    }

    pub fn deselect_all(&mut self) {
        self.clear_multi_selected();
        self.visual_range = None;
        self.range_anchor = None;
    }

    pub(crate) fn action_paths(&self) -> Vec<PathBuf> {
        if self.menu_open && self.context_menu_kind == FilePickerContextMenuKind::Tree {
            return self.context_menu_target.clone().into_iter().collect();
        }
        if self.focus == FilePickerFocus::Tree {
            return self.tree_cursor_path().map(Path::to_path_buf).into_iter().collect();
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
        // Context targeting is deliberately separate from persistent marking.
        // Moving the cursor gives the menu a concrete one-item target when the
        // row is unmarked; action_paths() still expands to the marked set when
        // the clicked row is already part of that set.
        self.set_file_cursor(index, self.file_visible_rows());
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
                self.bookmarks.replace_entries(commit.entries);
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
                self.bookmarks.replace_entries(commit.entries);
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

    pub fn sort_reverse(&self) -> bool {
        self.sort_reverse
    }

    /// True only after the user changes sorting in this picker session.
    pub fn sort_changed(&self) -> bool {
        self.sort_changed
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
        match crate::source_guard::recover_interrupted_verified_removals_once(&self.current_dir) {
            Ok(report) => {
                for restored in report.restored {
                    log::warn!(
                        "restored copy-undo removal interrupted before deletion: {}",
                        restored.display(),
                    );
                }
                let mut retained_messages = Vec::new();
                for (retained, reason) in report.retained {
                    log::error!(
                        "retained interrupted copy-undo recovery state at {}: {reason}",
                        retained.display(),
                    );
                    retained_messages.push(format!("{}: {reason}", retained.display()));
                }
                if !retained_messages.is_empty() {
                    self.set_error(FilePickerError::Io {
                        op: "recover interrupted copy undo",
                        path: self.current_dir.clone(),
                        message: retained_messages.join("; "),
                    });
                }
            }
            Err(error) => log::error!(
                "could not scan {} for interrupted copy-undo recovery state: {error}",
                self.current_dir.display(),
            ),
        }
        let read_dir = match fs::read_dir(&self.current_dir) {
            Ok(read_dir) => read_dir,
            Err(err) => {
                self.set_error(io_error("read directory", &self.current_dir, err));
                self.last_selection_dropped_invisible = self
                    .last_selection_dropped_invisible
                    .saturating_add(self.multi_selected.len());
                self.clear_multi_selected();
                self.range_anchor = None;
                self.visual_range = None;
                self.file_cursor = 0;
                self.file_scroll = 0;
                self.selected = None;
                self.visible_path_indices.clear();
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
        self.visible_path_indices = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.path.clone(), index))
            .collect();
        let visible_paths: HashSet<PathBuf> = self.entries.iter().map(|entry| entry.path.clone()).collect();
        let before = self.multi_selected.len();
        self.retain_multi_selected(|path| visible_paths.contains(path));
        let dropped = before.saturating_sub(self.multi_selected.len());
        self.last_selection_dropped_invisible = self
            .last_selection_dropped_invisible
            .saturating_add(dropped);
        if self
            .range_anchor
            .as_ref()
            .is_some_and(|path| !visible_paths.contains(path))
        {
            self.range_anchor = None;
        }
        if self
            .visual_range
            .as_ref()
            .is_some_and(|visual| !visible_paths.contains(&visual.anchor))
        {
            self.visual_range = None;
        }
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
        self.range_anchor = None;
        self.visual_range = None;
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
        self.range_anchor = None;
        self.visual_range = None;
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
        self.commit_visual_range();
        self.last_selection_ignored_directories = 0;
        if self.selection_mode == FilePickerSelectionMode::Directories {
            return FilePickerAction::Selected(self.current_dir.clone());
        }

        let (marked_files, ignored_directories) = self.marked_files_in_visible_order();
        self.last_selection_ignored_directories = ignored_directories;
        if !marked_files.is_empty() {
            return FilePickerAction::SelectedMany(marked_files);
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
        self.sort_changed = true;
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
        self.replace_multi_selected(destinations.iter().cloned());
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
        let rename_mode = rename_no_replace(&source, &destination).map_err(|err| {
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
        let mut committed_warnings = if self.operation_policy.verbose_degrade_notices {
            rename_mode
                .degraded_warning()
                .map(str::to_string)
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if let Err(err) = sync_directory(&parent) {
            committed_warnings.push(format!(
                "parent-directory synchronization failed: {err}"
            ));
        }
        if !committed_warnings.is_empty() {
            // The rename is already visible and must not be presented as an
            // uncommitted failure. Keep the repaired in-memory state and surface
            // any degraded-capability or durability warning while returning
            // success to close the editor.
            self.set_error(committed_operation_warning(
                &source,
                &destination,
                format!("rename committed: {}", committed_warnings.join("; ")),
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
        let clipboard =
            FilesystemClipboard::new(FilePickerClipboardMode::Cut, paths)
                .ok_or(FilePickerError::NoSelection)?;
        crate::text_input::mirror_host_clipboard_text(&clipboard.text_projection());
        self.clipboard = Some(clipboard);
        self.paste_retry_plan = None;
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
        let clipboard =
            FilesystemClipboard::new(FilePickerClipboardMode::Copy, paths)
                .ok_or(FilePickerError::NoSelection)?;
        crate::text_input::mirror_host_clipboard_text(&clipboard.text_projection());
        self.clipboard = Some(clipboard);
        self.paste_retry_plan = None;
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
        let target = self.filesystem_paste_target();
        self.try_paste_clipboard_to(&target)
    }

    /// Resolve the filesystem-paste destination from the pane that owns focus.
    ///
    /// The files pane pastes into the directory being browsed. The tree pane
    /// pastes into the selected tree directory, matching the Tree context-menu
    /// route. Text-entry and modal surfaces fall back to `current_dir`; hosts
    /// call this method only after focused text fields have declined a terminal
    /// paste event.
    #[must_use]
    pub fn filesystem_paste_target(&self) -> PathBuf {
        if self.focus == FilePickerFocus::Tree {
            if let Some(path) = self.tree_cursor_path() {
                return path.to_path_buf();
            }
        }
        self.current_dir.clone()
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
        let retry_plan = self.paste_retry_plan.clone();
        let (plan, resume_existing_destinations) = plan_filesystem_paste_with_retry(
            &clipboard,
            target_dir,
            retry_plan.as_ref(),
        )?;
        // Preserve an exact retry plan while a resumed operation is in flight,
        // so a worker disconnect cannot lose the original destination mapping.
        // Fresh plans have no prior recovery identity to retain.
        if !resume_existing_destinations {
            self.paste_retry_plan = None;
        }
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
        // This standalone picker path has no authoritative completion report.
        // Keep the report-dependent close-on-success control host-only.
        progress.set_auto_close_available(false);
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
            plan: plan.clone(),
            retry_plan: retry_plan.clone(),
        });
        let policy = self.operation_policy;
        thread::spawn(move || {
            run_picker_paste_worker(
                plan,
                policy,
                retry_plan,
                control,
                sender,
            )
        });
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
        let task_plan = task.plan.clone();
        let task_retry_plan = task.retry_plan.clone();
        let (completed, remaining_sources, failure_retry_plan, warnings, error) = match result {
            Ok(success) => (success.mappings, Vec::new(), None, success.warnings, None),
            Err(failure) => (
                failure.completed,
                failure.remaining_sources,
                failure.retry_plan,
                failure.warnings,
                Some(failure.error),
            ),
        };
        let remaining_sources = remaining_sources
            .into_iter()
            .filter(|source| fs::symlink_metadata(source).is_ok())
            .collect::<Vec<_>>();
        self.paste_retry_plan = if clipboard.mode() == FilePickerClipboardMode::Cut
            && !remaining_sources.is_empty()
        {
            let prior_retry = failure_retry_plan
                .as_ref()
                .or(task_retry_plan.as_ref());
            retry_plan_for_sources(
                &task_plan,
                &remaining_sources,
                prior_retry,
                None,
                None,
            )
        } else {
            None
        };
        let completed_sources = completed
            .iter()
            .map(|mapping| mapping.source.clone())
            .collect::<Vec<_>>();
        let completed_destinations = completed
            .iter()
            .map(|mapping| mapping.destination.clone())
            .collect::<Vec<_>>();
        let incomplete_navigation_mapping = error.as_ref().and_then(|error| match error {
            FilePickerError::DestinationCommittedMoveIncomplete {
                source,
                destination,
                ..
            } if fs::symlink_metadata(source).is_err()
                && fs::symlink_metadata(destination).is_ok() =>
            {
                Some(PasteMapping {
                    source: source.clone(),
                    destination: destination.clone(),
                })
            }
            _ => None,
        });
        let mut remapped_current = if clipboard.mode() == FilePickerClipboardMode::Cut
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
        if let Some(mapping) = incomplete_navigation_mapping.as_ref() {
            if let Some(incomplete_clipboard) = FilesystemClipboard::new(
                FilePickerClipboardMode::Cut,
                vec![mapping.source.clone()],
            ) {
                // History repair is independent for every affected root. A
                // completed root may already have remapped the current
                // directory, but that must not suppress repair of history
                // entries beneath a separately, partially deleted root.
                for path in self
                    .history_back
                    .iter_mut()
                    .chain(self.history_forward.iter_mut())
                {
                    if let Some(remapped) = crate::remap_path_after_cut(
                        path,
                        &incomplete_clipboard,
                        std::slice::from_ref(&mapping.destination),
                    ) {
                        *path = remapped;
                    }
                }
                if remapped_current.is_none() {
                    remapped_current = crate::remap_path_after_cut(
                        &self.current_dir,
                        &incomplete_clipboard,
                        std::slice::from_ref(&mapping.destination),
                    );
                }
            }
        }

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
        if let Some(mapping) = incomplete_navigation_mapping.as_ref() {
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
            self.replace_multi_selected(completed_destinations.iter().cloned());
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
            crate::FileTaskUserAction::ToggleAutoClose(_) => {
                // The standalone picker has no persisted host setting. The
                // progress state has already applied the local toggle.
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
                    self.retain_multi_selected(|selected| !same_path(selected, &path));
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

    pub(crate) fn context_text_input(&self) -> Option<&TextInputState> {
        match self.context_menu_kind {
            FilePickerContextMenuKind::Address => Some(&self.address_input),
            FilePickerContextMenuKind::NameEditor => Some(&self.create_name_input),
            FilePickerContextMenuKind::SaveNameEditor => Some(&self.save_name_input),
            FilePickerContextMenuKind::SearchEditor => Some(&self.search.input),
            FilePickerContextMenuKind::BookmarkNameEditor => Some(&self.bookmarks.name_input),
            _ => None,
        }
    }

    pub(crate) fn context_text_input_mut(&mut self) -> Option<&mut TextInputState> {
        match self.context_menu_kind {
            FilePickerContextMenuKind::Address => Some(&mut self.address_input),
            FilePickerContextMenuKind::NameEditor => Some(&mut self.create_name_input),
            FilePickerContextMenuKind::SaveNameEditor => Some(&mut self.save_name_input),
            FilePickerContextMenuKind::SearchEditor => Some(&mut self.search.input),
            FilePickerContextMenuKind::BookmarkNameEditor => Some(&mut self.bookmarks.name_input),
            _ => None,
        }
    }

    pub(crate) fn apply_path_case_transform(
        &mut self,
        action: FilePickerMenuAction,
    ) -> Result<usize, FilePickerError> {
        let paths = self.action_paths();
        if paths.is_empty() {
            return Err(FilePickerError::NoSelection);
        }
        let title_case = self.title_case;
        let transform = |name: &str| match action {
            FilePickerMenuAction::RenameTitleCase => title_case(name),
            FilePickerMenuAction::RenameUppercase => name.to_uppercase(),
            FilePickerMenuAction::RenameLowercase => name.to_lowercase(),
            _ => name.to_string(),
        };
        let destinations = execute_picker_case_rename_transaction(&paths, transform)?;
        if destinations.is_empty() {
            return Ok(0);
        }
        self.replace_multi_selected(destinations.iter().cloned());
        self.selected = destinations.last().cloned();
        self.refresh();
        if let Some(path) = self.selected.clone() {
            self.select_path_in_entries(&path);
        }
        self.select_tree_node_for_current_dir();
        Ok(destinations.len())
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
                ("Sort     ▸", Entry::SortSubmenu),
                ("Bookmarks", Entry::Action(Action::OpenBookmarks)),
                ("Cut", Entry::Action(Action::Cut)),
                ("Copy", Entry::Action(Action::Copy)),
                ("Paste", Entry::Action(Action::Paste)),
                ("Delete", Entry::Action(Action::Delete)),
            ],
            Kind::Address
            | Kind::NameEditor
            | Kind::SaveNameEditor
            | Kind::SearchEditor
            | Kind::BookmarkNameEditor => vec![
                ("Paste", Entry::Action(Action::TextPaste)),
                ("Copy", Entry::Action(Action::TextCopy)),
                ("Cut", Entry::Action(Action::TextCut)),
                ("Delete", Entry::Action(Action::TextDelete)),
                ("Select All", Entry::Action(Action::TextSelectAll)),
                ("Fix capitalization ▸", Entry::CaseSubmenu),
            ],
            Kind::Tree => vec![
                ("New      ▸", Entry::NewSubmenu),
                ("Add bookmark", Entry::Action(Action::AddBookmark)),
                ("Cut", Entry::Action(Action::Cut)),
                ("Copy", Entry::Action(Action::Copy)),
                ("Paste", Entry::Action(Action::Paste)),
                ("Rename   ▸", Entry::RenameSubmenu),
                ("Delete", Entry::Action(Action::Delete)),
            ],
            Kind::File => vec![
                ("Cut", Entry::Action(Action::Cut)),
                ("Copy", Entry::Action(Action::Copy)),
                ("Paste", Entry::Action(Action::Paste)),
                ("Rename   ▸", Entry::RenameSubmenu),
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

    pub(crate) fn submenu_entries(&self) -> Vec<(&'static str, FilePickerSubmenuEntry)> {
        use FilePickerMenuAction as Action;
        use FilePickerSubmenuEntry as Entry;
        match self.submenu_kind {
            FilePickerSubmenuKind::New => vec![
                ("File", Entry::Action(Action::NewFile)),
                ("Folder", Entry::Action(Action::NewFolder)),
            ],
            FilePickerSubmenuKind::Selection => vec![
                ("Select All", Entry::Action(Action::SelectAll)),
                ("Invert", Entry::Action(Action::InvertSelection)),
                ("Deselect All", Entry::Action(Action::DeselectAll)),
            ],
            FilePickerSubmenuKind::Sort => vec![
                ("Name", Entry::Action(Action::SortName)),
                ("Size", Entry::Action(Action::SortSize)),
                ("Type", Entry::Action(Action::SortType)),
                ("Modified", Entry::Action(Action::SortModified)),
            ],
            FilePickerSubmenuKind::Rename => vec![
                ("Rename", Entry::Action(Action::Rename)),
                ("Fix capitalization ▸", Entry::CaseSubmenu),
            ],
            FilePickerSubmenuKind::TextCase => vec![
                ("Title Case", Entry::Action(Action::TextTitleCase)),
                ("UPPERCASE", Entry::Action(Action::TextUppercase)),
                ("lowercase", Entry::Action(Action::TextLowercase)),
            ],
        }
    }

    pub(crate) fn nested_case_entries(
        &self,
    ) -> [(&'static str, FilePickerMenuAction); 3] {
        [
            ("Title Case", FilePickerMenuAction::RenameTitleCase),
            ("UPPERCASE", FilePickerMenuAction::RenameUppercase),
            ("lowercase", FilePickerMenuAction::RenameLowercase),
        ]
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
            FilePickerMenuAction::SortName
            | FilePickerMenuAction::SortSize
            | FilePickerMenuAction::SortType
            | FilePickerMenuAction::SortModified => true,
            FilePickerMenuAction::TextCut => self
                .context_text_input()
                .is_some_and(TextInputState::has_selection),
            FilePickerMenuAction::TextCopy => self
                .context_text_input()
                .is_some_and(TextInputState::has_selection),
            FilePickerMenuAction::TextPaste => self
                .context_text_input()
                .is_some_and(TextInputState::can_paste),
            FilePickerMenuAction::TextDelete => self.context_text_input().is_some_and(|input| {
                input.has_selection() || input.cursor < input.text.len()
            }),
            FilePickerMenuAction::TextSelectAll
            | FilePickerMenuAction::TextTitleCase
            | FilePickerMenuAction::TextUppercase
            | FilePickerMenuAction::TextLowercase => self
                .context_text_input()
                .is_some_and(|input| !input.text.is_empty()),
            FilePickerMenuAction::RenameTitleCase
            | FilePickerMenuAction::RenameUppercase
            | FilePickerMenuAction::RenameLowercase => !action_paths.is_empty(),
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

/// Authoritative copy-time and post-publication evidence retained for one
/// incomplete move. The source manifest owns the copy-time digest/tree proof;
/// the destination manifest owns the identity of the exact published objects
/// that passed the single content-verification traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MoveRecoveryProof {
    source_manifest: crate::SourceManifest,
    destination_manifest: crate::DestinationManifest,
}

/// Exact recovery token for an incomplete cut/move.
///
/// The public plan preserves source-to-destination identity. Private per-root
/// evidence lets the executor reuse already-established source and destination
/// proofs without recopying. Strict mounts can reuse retained destination identity;
/// reduced-semantics mounts perform one irreducible destination rehash before
/// destructive cleanup. Callers should treat this value as opaque and pass it back to
/// [`paste_filesystem_clipboard_with_retry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteRetryPlan {
    plan: PastePlan,
    recovery_by_source: std::collections::BTreeMap<PathBuf, MoveRecoveryProof>,
}

impl PasteRetryPlan {
    /// Create an exact-mapping retry token without retained proof evidence.
    /// This compatibility constructor still prevents destination suffixing;
    /// tokens returned by `PasteFailure` additionally reuse authoritative
    /// copy-time proof and are therefore the preferred recovery path.
    pub fn from_plan(plan: PastePlan) -> Self {
        Self {
            plan,
            recovery_by_source: std::collections::BTreeMap::new(),
        }
    }

    pub fn plan(&self) -> &PastePlan {
        &self.plan
    }

    pub fn mappings(&self) -> &[PasteMapping] {
        &self.plan.mappings
    }

    fn recovery_for(&self, source: &Path) -> Option<&MoveRecoveryProof> {
        self.recovery_by_source.get(source)
    }
}

impl std::ops::Deref for PasteRetryPlan {
    type Target = PastePlan;

    fn deref(&self) -> &Self::Target {
        &self.plan
    }
}

impl From<PastePlan> for PasteRetryPlan {
    fn from(plan: PastePlan) -> Self {
        Self::from_plan(plan)
    }
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

/// Structured partial failure. `completed` contains only roots whose requested
/// copy or move semantics completed successfully. `remaining_sources` contains
/// roots that can be retried, including a source retained after destination
/// publication failed verification; published-but-unverified destinations are
/// deliberately not represented as completed mappings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteFailure {
    pub completed: Vec<PasteMapping>,
    pub remaining_sources: Vec<PathBuf>,
    /// Exact source-to-destination mappings that may be supplied to
    /// `paste_filesystem_clipboard_with_retry` for an idempotent retry.
    /// This is populated only for retained cut sources whose original
    /// destination mapping is still authoritative.
    pub retry_plan: Option<PasteRetryPlan>,
    pub warnings: Vec<PasteWarning>,
    pub error: FilePickerError,
}

fn classify_paste_root_result(
    result: Result<(), FilePickerError>,
) -> Result<Option<String>, FilePickerError> {
    match result {
        Ok(()) => Ok(None),
        Err(FilePickerError::OperationCommittedWithWarning { message, .. }) => {
            Ok(Some(message))
        }
        Err(error) => Err(error),
    }
}

impl fmt::Display for PasteFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for PasteFailure {}

fn plan_filesystem_paste_with_retry(
    clipboard: &FilesystemClipboard,
    destination_dir: &Path,
    retry: Option<&PasteRetryPlan>,
) -> Result<(PastePlan, bool), FilePickerError> {
    if let Some(retry) = retry {
        if retry_plan_matches(retry, clipboard, destination_dir) {
            return Ok((retry.plan.clone(), true));
        }
        return Err(FilePickerError::WrongSelectionMode(
            "retry plan does not match this cut clipboard and destination directory",
        ));
    }
    plan_filesystem_paste(clipboard, destination_dir).map(|plan| (plan, false))
}

fn retry_plan_matches(
    retry: &PasteRetryPlan,
    clipboard: &FilesystemClipboard,
    destination_dir: &Path,
) -> bool {
    retry.plan.mode == FilePickerClipboardMode::Cut
        && clipboard.mode() == FilePickerClipboardMode::Cut
        && retry.plan.mappings.len() == clipboard.paths().len()
        && retry
            .plan
            .mappings
            .iter()
            .zip(clipboard.paths())
            .all(|(mapping, source)| {
                mapping.source.as_path() == source.as_path()
                    && mapping
                        .destination
                        .parent()
                        .is_some_and(|parent| same_path(parent, destination_dir))
            })
}

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
/// This starts a new operation plan. If it returns `PasteFailure` with a
/// `retry_plan`, pass that plan to `paste_filesystem_clipboard_with_retry` on
/// the next attempt so an already-published destination is verified and reused
/// rather than renamed to a duplicate destination.
/// Execute caller-supplied source-to-destination mappings exactly.
///
/// Unlike [`plan_filesystem_paste`], this never suffixes or otherwise rewrites
/// a destination. It is intended for replaying a previously completed file
/// operation (undo/redo) after the caller has revalidated its retained
/// manifests. The normal rename-first/copy-verify-delete engine remains the
/// sole mutation path, including cross-device move recovery.
pub fn execute_exact_paste_plan(
    plan: &PastePlan,
    policy: FileOperationPolicy,
) -> Result<PasteSuccess, PasteFailure> {
    if plan.mappings.is_empty() {
        return Ok(PasteSuccess {
            mappings: Vec::new(),
            warnings: Vec::new(),
        });
    }
    for mapping in &plan.mappings {
        if fs::symlink_metadata(&mapping.source).is_err() {
            return Err(PasteFailure {
                completed: Vec::new(),
                remaining_sources: plan
                    .mappings
                    .iter()
                    .map(|mapping| mapping.source.clone())
                    .collect(),
                retry_plan: None,
                warnings: Vec::new(),
                error: FilePickerError::ClipboardSourceMissing(mapping.source.clone()),
            });
        }
        let Some(parent) = mapping.destination.parent() else {
            return Err(PasteFailure {
                completed: Vec::new(),
                remaining_sources: plan
                    .mappings
                    .iter()
                    .map(|mapping| mapping.source.clone())
                    .collect(),
                retry_plan: None,
                warnings: Vec::new(),
                error: FilePickerError::NotADirectory(mapping.destination.clone()),
            });
        };
        if !parent.is_dir() {
            return Err(PasteFailure {
                completed: Vec::new(),
                remaining_sources: plan
                    .mappings
                    .iter()
                    .map(|mapping| mapping.source.clone())
                    .collect(),
                retry_plan: None,
                warnings: Vec::new(),
                error: FilePickerError::NotADirectory(parent.to_path_buf()),
            });
        }
        if fs::symlink_metadata(&mapping.destination).is_ok() {
            return Err(PasteFailure {
                completed: Vec::new(),
                remaining_sources: plan
                    .mappings
                    .iter()
                    .map(|mapping| mapping.source.clone())
                    .collect(),
                retry_plan: None,
                warnings: Vec::new(),
                error: FilePickerError::DestinationExists(mapping.destination.clone()),
            });
        }
    }

    let mut progress =
        |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
    execute_paste_plan_progress_with_resume(plan, policy, None, &mut progress)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactPasteRootProof {
    pub mapping: PasteMapping,
    pub proof: crate::FileTaskRootProof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactPasteProofSuccess {
    pub mappings: Vec<PasteMapping>,
    pub proofs: Vec<ExactPasteRootProof>,
    pub warnings: Vec<PasteWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactPasteProofFailure {
    pub completed: Vec<PasteMapping>,
    pub completed_proofs: Vec<ExactPasteRootProof>,
    /// Mappings whose destination publication committed but whose
    /// operation-time proof could not be returned. Callers must treat these as
    /// terminal and must not retry them from stale undo/redo history.
    pub committed_unverified: Vec<PasteMapping>,
    pub remaining_sources: Vec<PathBuf>,
    pub warnings: Vec<PasteWarning>,
    pub error: FilePickerError,
}

impl fmt::Display for ExactPasteProofFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for ExactPasteProofFailure {}

/// Execute an exact replay plan and return worker-owned operation-time proofs.
///
/// Unlike the compatibility API, every successful root carries the authority
/// produced inside the mutation engine: copy proofs come from the streaming
/// copy/publication verifier, copy-then-delete moves reuse that same recovery
/// proof, and native moves combine a pre-rename manifest with retained-handle
/// destination verification. Callers must run this potentially recursive work
/// off their interactive reducer thread.
pub fn execute_exact_paste_plan_with_proofs(
    plan: &PastePlan,
    policy: FileOperationPolicy,
) -> Result<ExactPasteProofSuccess, ExactPasteProofFailure> {
    execute_exact_paste_plan_with_proofs_internal(plan, policy, None)
}

/// Execute an exact replay plan while requiring each source to match the
/// operation-time proof supplied by the undo journal. The worker verifies the
/// source manifest it is already producing before publication or source
/// cleanup, avoiding both a pathname verification race and a duplicate hash
/// pass.
pub fn execute_exact_paste_plan_with_proofs_and_expected_sources(
    plan: &PastePlan,
    policy: FileOperationPolicy,
    expected_sources: &[crate::FileTaskRootProof],
) -> Result<ExactPasteProofSuccess, ExactPasteProofFailure> {
    if expected_sources.len() != plan.mappings.len() {
        return Err(ExactPasteProofFailure {
            completed: Vec::new(),
            completed_proofs: Vec::new(),
            committed_unverified: Vec::new(),
            remaining_sources: plan
                .mappings
                .iter()
                .map(|mapping| mapping.source.clone())
                .collect(),
            warnings: Vec::new(),
            error: FilePickerError::Io {
                op: "validate exact replay authority",
                path: PathBuf::new(),
                message: format!(
                    "expected {} source proofs for {} mappings",
                    expected_sources.len(),
                    plan.mappings.len(),
                ),
            },
        });
    }
    execute_exact_paste_plan_with_proofs_internal(plan, policy, Some(expected_sources))
}

fn execute_exact_paste_plan_with_proofs_internal(
    plan: &PastePlan,
    policy: FileOperationPolicy,
    expected_sources: Option<&[crate::FileTaskRootProof]>,
) -> Result<ExactPasteProofSuccess, ExactPasteProofFailure> {
    if let Err(failure) = preflight_exact_paste_plan(plan) {
        return Err(failure);
    }
    let mut progress =
        |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
    let mut completed = Vec::new();
    let mut proofs = Vec::new();
    let mut warnings = Vec::new();
    let mut io = crate::FileOperationIoCounters::default();

    for (index, mapping) in plan.mappings.iter().enumerate() {
        let expected_source = expected_sources.map(|proofs| &proofs[index]);
        let mut root_proof = None;
        let result = match plan.mode {
            FilePickerClipboardMode::Copy => {
                match safe_copy_path_progress_with_notices_accounted_with_expected(
                    &mapping.source,
                    &mapping.destination,
                    policy,
                    &mut progress,
                    &mut io,
                    expected_source,
                ) {
                    Ok(outcome) => {
                        root_proof = Some(crate::FileTaskRootProof {
                            source_manifest: outcome.source_manifest,
                            destination_manifest: outcome.destination_manifest,
                        });
                        if outcome.notices.is_empty() {
                            Ok(())
                        } else {
                            Err(committed_operation_warning(
                                &mapping.source,
                                &mapping.destination,
                                outcome.notices.join("; "),
                            ))
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            FilePickerClipboardMode::Cut => {
                let mut recovery = None;
                let result = move_path_with_policy_progress_accounted_with_recovery_and_expected(
                    &mapping.source,
                    &mapping.destination,
                    policy,
                    &mut progress,
                    &mut recovery,
                    expected_source,
                    &mut io,
                );
                if let Some(recovery) = recovery {
                    root_proof = Some(crate::FileTaskRootProof {
                        source_manifest: recovery.source_manifest,
                        destination_manifest: recovery.destination_manifest,
                    });
                }
                result
            }
        };

        match classify_paste_root_result(result) {
            Ok(warning) => {
                let Some(proof) = root_proof else {
                    let remaining_sources = plan.mappings[index..]
                        .iter()
                        .map(|mapping| mapping.source.clone())
                        .collect();
                    return Err(ExactPasteProofFailure {
                        completed,
                        completed_proofs: proofs,
                        committed_unverified: vec![mapping.clone()],
                        remaining_sources,
                        warnings,
                        error: FilePickerError::OperationCommittedButUnverified {
                            source: mapping.source.clone(),
                            destination: mapping.destination.clone(),
                            message: "operation committed without returning authoritative replay proof".to_string(),
                        },
                    });
                };
                completed.push(mapping.clone());
                proofs.push(ExactPasteRootProof {
                    mapping: mapping.clone(),
                    proof,
                });
                if let Some(message) = warning {
                    warnings.push(PasteWarning {
                        mapping: mapping.clone(),
                        message,
                    });
                }
            }
            Err(error) => {
                let remaining_sources = plan.mappings[index..]
                    .iter()
                    .map(|mapping| mapping.source.clone())
                    .filter(|source| fs::symlink_metadata(source).is_ok())
                    .collect();
                let committed_unverified = if matches!(
                    &error,
                    FilePickerError::OperationCommittedButUnverified { .. }
                        | FilePickerError::DestinationCommittedMoveIncomplete { .. }
                ) {
                    vec![mapping.clone()]
                } else {
                    Vec::new()
                };
                return Err(ExactPasteProofFailure {
                    completed,
                    completed_proofs: proofs,
                    committed_unverified,
                    remaining_sources,
                    warnings,
                    error,
                });
            }
        }
    }

    Ok(ExactPasteProofSuccess {
        mappings: completed,
        proofs,
        warnings,
    })
}

fn preflight_exact_paste_plan(
    plan: &PastePlan,
) -> Result<(), ExactPasteProofFailure> {
    for mapping in &plan.mappings {
        let failure = if fs::symlink_metadata(&mapping.source).is_err() {
            Some(FilePickerError::ClipboardSourceMissing(mapping.source.clone()))
        } else if mapping.destination.parent().is_none() {
            Some(FilePickerError::NotADirectory(mapping.destination.clone()))
        } else if !mapping.destination.parent().expect("checked parent").is_dir() {
            Some(FilePickerError::NotADirectory(
                mapping.destination.parent().expect("checked parent").to_path_buf(),
            ))
        } else if fs::symlink_metadata(&mapping.destination).is_ok() {
            Some(FilePickerError::DestinationExists(mapping.destination.clone()))
        } else {
            None
        };
        if let Some(error) = failure {
            return Err(ExactPasteProofFailure {
                completed: Vec::new(),
                completed_proofs: Vec::new(),
                committed_unverified: Vec::new(),
                remaining_sources: plan
                    .mappings
                    .iter()
                    .map(|mapping| mapping.source.clone())
                    .collect(),
                warnings: Vec::new(),
                error,
            });
        }
    }
    Ok(())
}

pub fn paste_filesystem_clipboard(
    clipboard: &FilesystemClipboard,
    destination_dir: &Path,
    policy: FileOperationPolicy,
) -> Result<PasteSuccess, PasteFailure> {
    paste_filesystem_clipboard_with_retry(clipboard, destination_dir, policy, None)
}

/// Paste a shared filesystem clipboard, optionally resuming the exact mapping
/// returned by a prior `PasteFailure`.
///
/// A supplied retry plan is accepted only when it exactly matches the cut
/// clipboard and destination directory. Matching existing destinations are
/// content-verified before only the outstanding source cleanup is resumed.
pub fn paste_filesystem_clipboard_with_retry(
    clipboard: &FilesystemClipboard,
    destination_dir: &Path,
    policy: FileOperationPolicy,
    retry_plan: Option<&PasteRetryPlan>,
) -> Result<PasteSuccess, PasteFailure> {
    if let Some(retry_plan) = retry_plan {
        if !retry_plan_matches(retry_plan, clipboard, destination_dir) {
            return Err(PasteFailure {
                completed: Vec::new(),
                remaining_sources: clipboard.paths().to_vec(),
                retry_plan: None,
                warnings: Vec::new(),
                error: FilePickerError::WrongSelectionMode(
                    "retry plan does not match this cut clipboard and destination directory",
                ),
            });
        }
    }
    let (plan, _resume_existing_destinations) =
        plan_filesystem_paste_with_retry(clipboard, destination_dir, retry_plan).map_err(|error| {
            PasteFailure {
                completed: Vec::new(),
                remaining_sources: clipboard.paths().to_vec(),
                retry_plan: None,
                warnings: Vec::new(),
                error,
            }
        })?;
    let mut progress =
        |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
    execute_paste_plan_progress_with_resume(
        &plan,
        policy,
        retry_plan,
        &mut progress,
    )
}

fn run_picker_paste_worker(
    plan: PastePlan,
    policy: FileOperationPolicy,
    retry_plan: Option<PasteRetryPlan>,
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
    let result = execute_paste_plan_progress_with_resume(
        &plan,
        policy,
        retry_plan.as_ref(),
        &mut progress,
    );
    let _ = sender.send(PickerPasteMessage::Finished(result));
}

#[derive(Debug)]
struct PasteRootExecution {
    result: Result<(), FilePickerError>,
    recovery: Option<MoveRecoveryProof>,
}

#[cfg_attr(not(test), allow(dead_code))] // retained synchronous compatibility/test entry point
fn execute_paste_plan_progress(
    plan: &PastePlan,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
) -> Result<PasteSuccess, PasteFailure> {
    execute_paste_plan_progress_with_resume(plan, policy, None, progress)
}

fn execute_paste_plan_progress_with_resume(
    plan: &PastePlan,
    policy: FileOperationPolicy,
    retry_plan: Option<&PasteRetryPlan>,
    progress: &mut FileOperationProgress<'_>,
) -> Result<PasteSuccess, PasteFailure> {
    let mut io = crate::FileOperationIoCounters::default();
    execute_paste_plan_progress_with_resume_accounted(
        plan,
        policy,
        retry_plan,
        progress,
        &mut io,
    )
}

fn execute_paste_plan_progress_with_resume_accounted(
    plan: &PastePlan,
    policy: FileOperationPolicy,
    retry_plan: Option<&PasteRetryPlan>,
    progress: &mut FileOperationProgress<'_>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<PasteSuccess, PasteFailure> {
    execute_paste_plan_progress_with_recovery(plan, retry_plan, |mode, mapping| {
        let mut recovery = None;
        let result = match mode {
            FilePickerClipboardMode::Cut => {
                let retained_recovery =
                    retry_plan.and_then(|retry| retry.recovery_for(&mapping.source));
                let destination_exists = fs::symlink_metadata(&mapping.destination).is_ok();
                if retry_plan.is_some() && (retained_recovery.is_some() || destination_exists) {
                    copy_then_delete_progress_with_resume_accounted(
                        &mapping.source,
                        &mapping.destination,
                        policy,
                        progress,
                        true,
                        retained_recovery,
                        &mut recovery,
                        io,
                    )
                } else {
                    // An exact retry token can also contain roots that were never
                    // attempted. Preserve their reserved destination, but still
                    // take the normal rename-first O(1) path when it is absent.
                    move_path_with_policy_progress_accounted_with_recovery(
                        &mapping.source,
                        &mapping.destination,
                        policy,
                        progress,
                        &mut recovery,
                        io,
                    )
                }
            }
            FilePickerClipboardMode::Copy => {
                match safe_copy_path_progress_with_notices_accounted(
                    &mapping.source,
                    &mapping.destination,
                    policy,
                    progress,
                    io,
                ) {
                    Ok(mut outcome) => {
                        if let Some(control_error) = outcome.post_publication_control {
                            outcome.notices.push(format!(
                                "progress control changed after the verified copy completed: {}",
                                control_error.message()
                            ));
                        }
                        if outcome.notices.is_empty() {
                            Ok(())
                        } else {
                            Err(committed_operation_warning(
                                &mapping.source,
                                &mapping.destination,
                                outcome.notices.join("; "),
                            ))
                        }
                    }
                    Err(error) => Err(error),
                }
            }
        };
        PasteRootExecution { result, recovery }
    })
}

fn retry_plan_for_sources(
    plan: &PastePlan,
    remaining_sources: &[PathBuf],
    prior_retry: Option<&PasteRetryPlan>,
    current_source: Option<&Path>,
    current_recovery: Option<MoveRecoveryProof>,
) -> Option<PasteRetryPlan> {
    if plan.mode != FilePickerClipboardMode::Cut || remaining_sources.is_empty() {
        return None;
    }
    let mappings = plan
        .mappings
        .iter()
        .filter(|mapping| remaining_sources.iter().any(|source| source == &mapping.source))
        .cloned()
        .collect::<Vec<_>>();
    if mappings.is_empty() {
        return None;
    }

    let mut recovery_by_source = std::collections::BTreeMap::new();
    if let Some(prior_retry) = prior_retry {
        for mapping in &mappings {
            if let Some(recovery) = prior_retry.recovery_for(&mapping.source) {
                recovery_by_source.insert(mapping.source.clone(), recovery.clone());
            }
        }
    }
    if let (Some(source), Some(recovery)) = (current_source, current_recovery) {
        if remaining_sources.iter().any(|remaining| remaining == source) {
            recovery_by_source.insert(source.to_path_buf(), recovery);
        }
    }

    Some(PasteRetryPlan {
        plan: PastePlan {
            mode: FilePickerClipboardMode::Cut,
            mappings,
        },
        recovery_by_source,
    })
}

#[cfg_attr(not(test), allow(dead_code))] // retained synchronous compatibility/test entry point
fn execute_paste_plan_progress_with<F>(
    plan: &PastePlan,
    mut execute_root: F,
) -> Result<PasteSuccess, PasteFailure>
where
    F: FnMut(FilePickerClipboardMode, &PasteMapping) -> Result<(), FilePickerError>,
{
    execute_paste_plan_progress_with_recovery(plan, None, |mode, mapping| PasteRootExecution {
        result: execute_root(mode, mapping),
        recovery: None,
    })
}

fn execute_paste_plan_progress_with_recovery<F>(
    plan: &PastePlan,
    prior_retry: Option<&PasteRetryPlan>,
    mut execute_root: F,
) -> Result<PasteSuccess, PasteFailure>
where
    F: FnMut(FilePickerClipboardMode, &PasteMapping) -> PasteRootExecution,
{
    let mut completed = Vec::new();
    let mut retry_sources = Vec::new();
    let mut warnings = Vec::new();
    for (index, mapping) in plan.mappings.iter().enumerate() {
        let PasteRootExecution { result, recovery } = execute_root(plan.mode, mapping);
        match classify_paste_root_result(result) {
            Ok(None) => completed.push(mapping.clone()),
            Ok(Some(message)) => {
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
                let retry_plan = retry_plan_for_sources(
                    plan,
                    &retry_sources,
                    prior_retry,
                    Some(&mapping.source),
                    recovery,
                );
                return Err(PasteFailure {
                    completed,
                    remaining_sources: retry_sources,
                    retry_plan,
                    warnings,
                    error,
                });
            }
        }
    }
    if retry_sources.is_empty() {
        Ok(PasteSuccess { mappings: completed, warnings })
    } else {
        let retry_plan = retry_plan_for_sources(
            plan,
            &retry_sources,
            prior_retry,
            None,
            None,
        );
        Err(PasteFailure {
            completed,
            remaining_sources: retry_sources,
            retry_plan,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitialMoveRoute {
    NativeRename,
    CopyThenDelete,
}

fn initial_move_route(
    _source_capabilities: crate::FilesystemCapabilities,
    _destination_capabilities: crate::FilesystemCapabilities,
    force_copy_then_delete: bool,
) -> InitialMoveRoute {
    if force_copy_then_delete {
        InitialMoveRoute::CopyThenDelete
    } else {
        // Capability limitations select the proof used after rename. They do
        // not make recursive copying the first choice.
        InitialMoveRoute::NativeRename
    }
}

#[cfg(test)]
std::thread_local! {
    static TEST_FORCE_COPY_THEN_DELETE_MOVE: std::cell::Cell<bool> = std::cell::Cell::new(false);
    static TEST_STOP_MOVE_AFTER_VERIFIED_PUBLICATION: std::cell::Cell<bool> = std::cell::Cell::new(false);
}

#[cfg(test)]
fn take_test_force_copy_then_delete_move() -> bool {
    TEST_FORCE_COPY_THEN_DELETE_MOVE.with(|flag| flag.replace(false))
}

#[cfg(not(test))]
fn take_test_force_copy_then_delete_move() -> bool {
    false
}

#[cfg(test)]
fn take_test_stop_move_after_verified_publication() -> bool {
    TEST_STOP_MOVE_AFTER_VERIFIED_PUBLICATION.with(|flag| flag.replace(false))
}

#[cfg(not(test))]
fn take_test_stop_move_after_verified_publication() -> bool {
    false
}

#[cfg_attr(not(test), allow(dead_code))] // retained synchronous compatibility/test entry point
fn move_path_with_policy(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
) -> Result<(), FilePickerError> {
    let mut progress = |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
    move_path_with_policy_progress(source, destination, policy, &mut progress)
}

#[cfg_attr(not(test), allow(dead_code))] // retained synchronous compatibility/test entry point
fn move_path_with_policy_progress(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
) -> Result<(), FilePickerError> {
    let mut io = crate::FileOperationIoCounters::default();
    move_path_with_policy_progress_accounted(
        source,
        destination,
        policy,
        progress,
        &mut io,
    )
}

#[cfg_attr(not(test), allow(dead_code))] // retained synchronous compatibility/test entry point
fn move_path_with_policy_progress_accounted(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    let mut recovery = None;
    move_path_with_policy_progress_accounted_with_recovery(
        source,
        destination,
        policy,
        progress,
        &mut recovery,
        io,
    )
}

fn move_path_with_policy_progress_accounted_with_recovery(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    recovery_out: &mut Option<MoveRecoveryProof>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    move_path_with_policy_progress_accounted_with_recovery_and_expected(
        source,
        destination,
        policy,
        progress,
        recovery_out,
        None,
        io,
    )
}

fn move_path_with_policy_progress_accounted_with_recovery_and_expected(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    recovery_out: &mut Option<MoveRecoveryProof>,
    expected_source: Option<&crate::FileTaskRootProof>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    progress(source, destination, 0, false)?;
    let source_capabilities = crate::filesystem_capabilities(source);
    let initial_destination_capabilities = crate::filesystem_capabilities(destination);
    let route = initial_move_route(
        source_capabilities,
        initial_destination_capabilities,
        take_test_force_copy_then_delete_move(),
    );
    if route == InitialMoveRoute::CopyThenDelete {
        return copy_then_delete_progress_with_resume_accounted_expected(
            source,
            destination,
            policy,
            progress,
            false,
            None,
            recovery_out,
            expected_source,
            io,
        );
    }

    io.source_tree_walks = io.source_tree_walks.saturating_add(1);
    let source_manifest = crate::capture_manifest_with_mode(source, policy.verification).map_err(|error| FilePickerError::Io {
        op: "capture native-rename source manifest",
        path: source.to_path_buf(),
        message: error,
    })?;
    if policy.verification == VerificationMode::Strong {
        io.source_bytes_hashed = io
            .source_bytes_hashed
            .saturating_add(source_manifest.total_file_bytes());
    }
    if let Some(expected) = expected_source {
        expected
            .destination_manifest
            .verify_captured_replay_source(
                &expected.source_manifest,
                &source_manifest,
                source_capabilities,
            )
            .map_err(|message| FilePickerError::Io {
                op: "verify native-move replay authority",
                path: source.to_path_buf(),
                message,
            })?;
    }
    let rename_proof = crate::RenameSourceProof::capture(source)
        .map_err(|error| io_error("capture native-rename source proof", source, error))?;
    rename_proof
        .verify_manifest_root(&source_manifest, source_capabilities)
        .map_err(|message| FilePickerError::Io {
            op: "bind native-rename source authority",
            path: source.to_path_buf(),
            message,
        })?;
    io.rename_attempts = io.rename_attempts.saturating_add(1);
    match rename_no_replace_for_operation(
        source,
        destination,
        expected_source.is_some(),
    ) {
        Ok(rename_mode) => {
            if matches!(rename_mode, RenameNoReplaceMode::CheckedBestEffort) {
                io.rename_fallbacks = io.rename_fallbacks.saturating_add(1);
            }
            let destination_capabilities = crate::filesystem_capabilities(destination);
            let rename_verification = crate::verify_committed_rename(
                source,
                destination,
                &rename_proof,
                source_capabilities,
                destination_capabilities,
            )
            .map_err(|message| {
                committed_operation_unverified(
                    source,
                    destination,
                    format!(
                        "native rename committed, but the pathname transition could not be proven: {message}"
                    ),
                )
            })?;

            let destination_manifest = source_manifest
                .destination_identity_after_root_rename(
                    rename_verification.destination_snapshot.clone(),
                    destination_capabilities,
                )
                .map_err(|message| {
                    committed_operation_unverified(
                        source,
                        destination,
                        format!("native rename committed, but undo proof could not be assembled: {message}"),
                    )
                })?;
            *recovery_out = Some(MoveRecoveryProof {
                source_manifest,
                destination_manifest,
            });

            let mut warnings = if policy.verbose_degrade_notices {
                rename_mode
                    .degraded_warning()
                    .map(str::to_string)
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            if let Some(warning) = rename_verification.warning {
                warnings.push(warning);
            }
            if rename_verification.portable_evidence && policy.verbose_degrade_notices {
                warnings.push(
                    "native rename was accepted using retained-handle/type/size/path-transition evidence because stable inode or nanosecond timestamp semantics are unavailable"
                        .to_string(),
                );
            }
            if let Some(parent) = destination.parent() {
                if let Err(err) = sync_directory_accounted(parent, io) {
                    warnings.push(format!(
                        "destination parent directory synchronization failed: {err}"
                    ));
                }
            }
            if let Some(parent) = source.parent() {
                if destination.parent() != Some(parent) {
                    if let Err(err) = sync_directory_accounted(parent, io) {
                        warnings.push(format!(
                            "source parent directory synchronization failed: {err}"
                        ));
                    }
                }
            }
            if let Err(err) = progress(source, destination, 0, true) {
                warnings.push(format!(
                    "progress control changed after the move committed: {}",
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
                copy_then_delete_progress_with_resume_accounted_expected(
                    source,
                    destination,
                    policy,
                    progress,
                    false,
                    None,
                    recovery_out,
                    expected_source,
                    io,
                )
            }
        },
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            // Atomic and checked best-effort rename were both unavailable.
            // This is a same-operation capability failure rather than an
            // explicit-delete request, so the verified copy/quarantine path is
            // the safe functional fallback even when cross-device moves are
            // otherwise disabled.
            copy_then_delete_progress_with_resume_accounted_expected(
                source,
                destination,
                policy,
                progress,
                false,
                None,
                recovery_out,
                expected_source,
                io,
            )
        }
        Err(err) => Err(io_error("move", source, err)),
    }
}

#[cfg_attr(not(test), allow(dead_code))] // retained synchronous compatibility/test entry point
fn copy_then_delete_progress(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
) -> Result<(), FilePickerError> {
    let mut io = crate::FileOperationIoCounters::default();
    copy_then_delete_progress_accounted(source, destination, policy, progress, &mut io)
}

#[cfg_attr(not(test), allow(dead_code))] // retained synchronous compatibility/test entry point
fn copy_then_delete_progress_accounted(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    let mut recovery = None;
    copy_then_delete_progress_with_resume_accounted(
        source,
        destination,
        policy,
        progress,
        false,
        None,
        &mut recovery,
        io,
    )
}

fn copy_then_delete_progress_with_resume_accounted(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    resume_existing_destination: bool,
    retained_recovery: Option<&MoveRecoveryProof>,
    recovery_out: &mut Option<MoveRecoveryProof>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    copy_then_delete_progress_with_resume_accounted_expected(
        source,
        destination,
        policy,
        progress,
        resume_existing_destination,
        retained_recovery,
        recovery_out,
        None,
        io,
    )
}

fn copy_then_delete_progress_with_resume_accounted_expected(
    source: &Path,
    destination: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    resume_existing_destination: bool,
    retained_recovery: Option<&MoveRecoveryProof>,
    recovery_out: &mut Option<MoveRecoveryProof>,
    expected_source: Option<&crate::FileTaskRootProof>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    let identity_policy_warning = policy
        .verbose_degrade_notices
        .then(|| crate::filesystem_identity_policy_notice(source))
        .flatten();
    let destination_preexisted = match fs::symlink_metadata(destination) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(io_error(
                "inspect originally planned destination",
                destination,
                error,
            ));
        }
    };

    let copy_outcome = if resume_existing_destination && destination_preexisted {
        if let Some(retained_recovery) = retained_recovery {
            io.destination_tree_walks = io.destination_tree_walks.saturating_add(1);
            let mut destination_control_error = None;
            let verification = retained_recovery
                .destination_manifest
                .verify_reused_copy_at_with_cancel(
                    &retained_recovery.source_manifest,
                    destination,
                    |path| {
                        if destination_control_error.is_some() {
                            return false;
                        }
                        match progress(path, destination, 0, false) {
                            Ok(()) => true,
                            Err(error) => {
                                destination_control_error = Some(error);
                                false
                            }
                        }
                    },
                );
            if let Some(error) = destination_control_error {
                return Err(error);
            }
            let destination_bytes_rehashed = verification.map_err(|error| {
                destination_committed_move_incomplete(
                    source,
                    destination,
                    format!(
                        "the originally planned retry destination no longer matches the retained authoritative publication proof; source cleanup was not attempted: {error}"
                    ),
                )
            })?;
            io.destination_bytes_hashed = io
                .destination_bytes_hashed
                .saturating_add(destination_bytes_rehashed);
            VerifiedCopyOutcome {
                source_manifest: retained_recovery.source_manifest.clone(),
                destination_manifest: retained_recovery.destination_manifest.clone(),
                notices: vec![format!(
                    "retry reused the original verified publication proof for {}; no data was recopied{}",
                    destination.display(),
                    match (policy.verification, destination_bytes_rehashed) {
                        (VerificationMode::Standard, 0) =>
                            "; identity-level authority required no destination content read",
                        (_, 0) => " and strict mount evidence avoided a destination rehash",
                        _ => "; reduced mount identity required one destination verification rehash",
                    }
                )],
                post_publication_control: None,
            }
        } else {
            // Compatibility recovery tokens may preserve only the exact mapping.
            // In that case, establish fresh authority once without recopying.
            let mut manifest_control_error = None;
            io.source_tree_walks = io.source_tree_walks.saturating_add(1);
            let manifest_result = crate::capture_manifest_with_mode_and_cancel(
                source,
                policy.verification,
                |path| {
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
                },
            );
            if let Some(error) = manifest_control_error {
                return Err(error);
            }
            let source_manifest = manifest_result.map_err(|message| FilePickerError::Io {
                op: "capture retry source proof",
                path: source.to_path_buf(),
                message,
            })?;
            if policy.verification == VerificationMode::Strong {
                io.source_bytes_hashed = io
                    .source_bytes_hashed
                    .saturating_add(source_manifest.total_file_bytes());
                io.bytes_redundantly_rehashed = io
                    .bytes_redundantly_rehashed
                    .saturating_add(source_manifest.total_file_bytes());
            }
            io.destination_tree_walks = io.destination_tree_walks.saturating_add(1);
            let mut destination_control_error = None;
            let mut capture = |path: &Path| {
                    if destination_control_error.is_some() {
                        return false;
                    }
                    match progress(path, destination, 0, false) {
                        Ok(()) => true,
                        Err(error) => {
                            destination_control_error = Some(error);
                            false
                        }
                    }
                };
            let destination_manifest_result = if policy.verification == VerificationMode::Strong {
                io.destination_bytes_hashed = io
                    .destination_bytes_hashed
                    .saturating_add(source_manifest.total_file_bytes());
                source_manifest.capture_verified_copy_at_with_cancel(destination, &mut capture)
            } else {
                source_manifest.capture_identity_copy_at_with_cancel(destination, &mut capture)
            };
            if let Some(error) = destination_control_error {
                return Err(error);
            }
            let destination_manifest = destination_manifest_result.map_err(|error| {
                destination_committed_move_incomplete(
                    source,
                    destination,
                    format!(
                        "the originally planned retry destination differs from the retained source; source cleanup was not attempted: {error}"
                    ),
                )
            })?;
            VerifiedCopyOutcome {
                source_manifest,
                destination_manifest,
                notices: vec![format!(
                    "retry reused the originally planned destination {} after verifying it against the retained source; no data was recopied",
                    destination.display()
                )],
                post_publication_control: None,
            }
        }
    } else {
        safe_copy_path_progress_with_notices_accounted_with_expected(
            source,
            destination,
            policy,
            progress,
            io,
            expected_source,
        )?
    };

    if let Some(expected) = expected_source {
        expected
            .destination_manifest
            .verify_captured_replay_source(
                &expected.source_manifest,
                &copy_outcome.source_manifest,
                crate::filesystem_capabilities(source),
            )
            .map_err(|message| FilePickerError::Io {
                op: "verify copy-then-delete replay authority",
                path: source.to_path_buf(),
                message,
            })?;
    }

    let authoritative_recovery = MoveRecoveryProof {
        source_manifest: copy_outcome.source_manifest.clone(),
        destination_manifest: copy_outcome.destination_manifest.clone(),
    };

    if let Some(control_error) = copy_outcome.post_publication_control {
        let reason = match control_error {
            FilePickerError::OperationSkipped => {
                "the user skipped the move after the destination was published and verified"
                    .to_string()
            }
            FilePickerError::OperationCancelled => {
                "the user aborted the move after the destination was published and verified"
                    .to_string()
            }
            other => format!(
                "progress handling stopped the move after the destination was published and verified: {}",
                other.message()
            ),
        };
        let notices = if copy_outcome.notices.is_empty() {
            String::new()
        } else {
            format!("; copy notices: {}", copy_outcome.notices.join("; "))
        };
        *recovery_out = Some(authoritative_recovery.clone());
        return Err(destination_committed_move_incomplete(
            source,
            destination,
            format!(
                "{reason}; source cleanup did not begin, so the source remains in place{notices}"
            ),
        ));
    }

    if take_test_stop_move_after_verified_publication() {
        *recovery_out = Some(authoritative_recovery.clone());
        return Err(destination_committed_move_incomplete(
            source,
            destination,
            "test-injected stop after verified destination publication; source cleanup did not begin"
                .to_string(),
        ));
    }

    let (quarantine, quarantine_mode) =
        match quarantine_picker_source_accounted(source, io) {
            Ok(result) => result,
            Err(error) => {
                *recovery_out = Some(authoritative_recovery.clone());
                return Err(destination_committed_move_incomplete(
                    source,
                    destination,
                    format!("safe cleanup could not begin: {error}"),
                ))
            }
        };

    // Strong mode preserves the two-boundary durability proof: quarantine
    // publication and final source removal are synchronized separately.
    // Standard mode intentionally matches ordinary copy/move expectations and
    // synchronizes the source parent only after final removal.
    let mut quarantine_durability_warning = None;
    if policy.verification == VerificationMode::Strong {
        if let Some(parent) = source.parent() {
            if let Err(error) = sync_directory_accounted(parent, io) {
                quarantine_durability_warning = Some(format!(
                    "source was quarantined, but source-parent synchronization failed; cleanup continued in degraded durability mode: {error}"
                ));
            }
        }
    }

    io.source_tree_walks = io.source_tree_walks.saturating_add(1);
    let cleanup_outcome = match delete_verified_quarantine_progress_accounted(
        &quarantine,
        destination,
        &copy_outcome.source_manifest,
        &copy_outcome.destination_manifest,
        progress,
        io,
    ) {
        Ok(outcome) => outcome,
        Err(failure) => {
            let detail = if failure.deleted_entries == 0 {
                let (restored, recovery) = try_restore_picker_quarantine_accounted(
                    &quarantine,
                    source,
                    io,
                );
                if restored
                    && matches!(
                        &failure.error,
                        FilePickerError::OperationSkipped | FilePickerError::OperationCancelled
                    )
                {
                    *recovery_out = Some(authoritative_recovery.clone());
                }
                format!(
                    "verified source cleanup stopped before any quarantined entry was deleted: {}; {}",
                    failure.error.message(),
                    recovery
                )
            } else {
                format!(
                    "verified source cleanup stopped after deleting {} quarantined entr{}; undeleted remnants remain at {}: {}",
                    failure.deleted_entries,
                    if failure.deleted_entries == 1 { "y" } else { "ies" },
                    quarantine.display(),
                    failure.error.message()
                )
            };
            return Err(destination_committed_move_incomplete(
                source,
                destination,
                detail,
            ));
        }
    };

    let mut completion_warnings = copy_outcome.notices;
    completion_warnings.extend(quarantine_durability_warning);
    if let Some(control_error) = cleanup_outcome.post_completion_control {
        let message = match control_error {
            FilePickerError::OperationSkipped =>
                "a skip request arrived after the verified source root was already deleted; the move completed".to_string(),
            FilePickerError::OperationCancelled =>
                "an abort request arrived after the verified source root was already deleted; the move completed".to_string(),
            other => format!(
                "progress handling reported an error after the verified source root was already deleted; the move completed: {}",
                other.message()
            ),
        };
        completion_warnings.push(message);
    }
    if let Some(container) = quarantine.parent() {
        if let Err(err) = fs::remove_dir(container) {
            if err.kind() != io::ErrorKind::NotFound {
                completion_warnings.push(format!(
                    "verified source removed, but empty quarantine {} could not be removed: {err}",
                    container.display()
                ));
            }
        }
    }
    if let Some(parent) = source.parent() {
        if let Err(err) = sync_directory_accounted(parent, io) {
            completion_warnings.push(format!(
                "verified source removed, but source-parent synchronization failed: {err}"
            ));
        }
    }
    if policy.verbose_degrade_notices {
        completion_warnings.extend(quarantine_mode.degraded_warning().map(str::to_string));
    }
    completion_warnings.extend(identity_policy_warning);
    *recovery_out = Some(authoritative_recovery);
    if !completion_warnings.is_empty() {
        return Err(committed_operation_warning(
            source,
            destination,
            format!("move completed: {}", completion_warnings.join("; ")),
        ));
    }
    Ok(())
}

static PICKER_SOURCE_QUARANTINE_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn quarantine_picker_source_accounted(
    source: &Path,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(PathBuf, RenameNoReplaceMode), String> {
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
        io.rename_attempts = io.rename_attempts.saturating_add(1);
        match rename_no_replace(source, &quarantine) {
            Ok(mode) => {
                if matches!(mode, RenameNoReplaceMode::CheckedBestEffort) {
                    io.rename_fallbacks = io.rename_fallbacks.saturating_add(1);
                }
                return Ok((quarantine, mode));
            }
            Err(error) => {
                let _ = fs::remove_dir(&container);
                return Err(format!(
                    "could not quarantine {} without replacing another path: {error}",
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

fn try_restore_picker_quarantine_accounted(
    quarantine: &Path,
    original: &Path,
    io: &mut crate::FileOperationIoCounters,
) -> (bool, String) {
    io.rename_attempts = io.rename_attempts.saturating_add(1);
    match rename_no_replace(quarantine, original) {
        Ok(mode) => {
            if matches!(mode, RenameNoReplaceMode::CheckedBestEffort) {
                io.rename_fallbacks = io.rename_fallbacks.saturating_add(1);
            }
            let mut details = vec![format!("source restored to {}", original.display())];
            if let Some(warning) = mode.degraded_warning() {
                details.push(warning.to_string());
            }
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
                if let Err(error) = sync_directory_accounted(parent, io) {
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

#[derive(Debug)]
struct VerifiedQuarantineCleanupOutcome {
    /// A callback error observed only after the complete verified source root
    /// had already been removed. The move is complete; callers report this as
    /// a committed warning rather than an incomplete move.
    post_completion_control: Option<FilePickerError>,
}

#[derive(Debug)]
struct VerifiedQuarantineCleanupFailure {
    error: FilePickerError,
    /// Number of quarantined entries irreversibly removed before the failure.
    deleted_entries: usize,
}

fn delete_verified_quarantine_progress_accounted(
    root: &Path,
    destination: &Path,
    manifest: &crate::SourceManifest,
    destination_manifest: &crate::DestinationManifest,
    progress: &mut FileOperationProgress<'_>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<VerifiedQuarantineCleanupOutcome, VerifiedQuarantineCleanupFailure> {
    let mut deleted_entries = 0usize;
    let mut post_completion_control = None;
    io.destination_entry_verification_passes = io
        .destination_entry_verification_passes
        .saturating_add(1);
    match delete_verified_quarantine_entry_progress_accounted(
        root,
        root,
        destination,
        manifest,
        destination_manifest,
        progress,
        &mut deleted_entries,
        &mut post_completion_control,
        io,
    ) {
        Ok(()) => Ok(VerifiedQuarantineCleanupOutcome {
            post_completion_control,
        }),
        Err(error) => Err(VerifiedQuarantineCleanupFailure {
            error,
            deleted_entries,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn delete_verified_quarantine_entry_progress_accounted(
    root: &Path,
    path: &Path,
    destination: &Path,
    manifest: &crate::SourceManifest,
    destination_manifest: &crate::DestinationManifest,
    progress: &mut FileOperationProgress<'_>,
    deleted_entries: &mut usize,
    post_completion_control: &mut Option<FilePickerError>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    progress(path, destination, 0, false)?;
    let relative = path.strip_prefix(root).map_err(|_| FilePickerError::Io {
        op: "verify quarantined source path",
        path: path.to_path_buf(),
        message: "quarantine traversal escaped its root".to_string(),
    })?;
    let expected = manifest.expected_snapshot(relative).ok_or_else(|| FilePickerError::Io {
        op: "verify quarantined source manifest",
        path: path.to_path_buf(),
        message: "unplanned source entry appeared during cleanup".to_string(),
    })?;
    let destination_path = if relative.as_os_str().is_empty() {
        destination.to_path_buf()
    } else {
        destination.join(relative)
    };

    // Verify the quarantined source first. Destination stability is checked
    // afterward as the final gate immediately before this source entry is
    // removed, minimizing the unavoidable pathname-to-unlink race.

    if expected.kind() == crate::SourceKind::Directory {
        let source_bytes_rehashed = manifest
            .verify_cleanup_entry_at(relative, path)
            .map_err(|message| FilePickerError::Io {
                op: "verify quarantined source directory",
                path: path.to_path_buf(),
                message,
            })?;
        io.source_bytes_hashed = io
            .source_bytes_hashed
            .saturating_add(source_bytes_rehashed);
        let mut children = fs::read_dir(path)
            .map_err(|err| io_error("read quarantined source directory", path, err))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| io_error("read quarantined source entry", path, err))?;
        children.sort_by_key(|entry| entry.file_name());
        let actual_children = children
            .iter()
            .map(|entry| entry.file_name())
            .collect::<std::collections::BTreeSet<_>>();
        let expected_children = manifest.expected_direct_children(relative);
        if actual_children != expected_children {
            return Err(FilePickerError::Io {
                op: "verify quarantined source membership",
                path: path.to_path_buf(),
                message: "source tree membership changed after the copy-time proof was captured"
                    .to_string(),
            });
        }
        for child in children {
            delete_verified_quarantine_entry_progress_accounted(
                root,
                &child.path(),
                destination,
                manifest,
                destination_manifest,
                progress,
                deleted_entries,
                post_completion_control,
                io,
            )?;
        }
        let before_remove = crate::snapshot_path(path)
            .map_err(|error| io_error("re-identify source directory before removal", path, error))?;
        expected
            .verify_same_identity_with_policy(
                &before_remove,
                crate::filesystem_identity_policy(path),
            )
            .map_err(|message| FilePickerError::Io {
                op: "verify source directory identity before removal",
                path: path.to_path_buf(),
                message,
            })?;
        verify_destination_entry_before_source_deletion(
            path,
            &destination_path,
            relative,
            manifest,
            destination_manifest,
            progress,
            io,
        )?;
        fs::remove_dir(path)
            .map_err(|err| io_error("delete quarantined source directory", path, err))?;
    } else {
        let source_bytes_rehashed = manifest
            .verify_cleanup_entry_at(relative, path)
            .map_err(|message| FilePickerError::Io {
                op: "verify source content and identity immediately before deletion",
                path: path.to_path_buf(),
                message,
            })?;
        io.source_bytes_hashed = io
            .source_bytes_hashed
            .saturating_add(source_bytes_rehashed);
        verify_destination_entry_before_source_deletion(
            path,
            &destination_path,
            relative,
            manifest,
            destination_manifest,
            progress,
            io,
        )?;
        fs::remove_file(path)
            .map_err(|err| io_error("delete quarantined source file", path, err))?;
    }
    *deleted_entries = (*deleted_entries).saturating_add(1);

    match progress(path, destination, 0, true) {
        Ok(()) => Ok(()),
        Err(error) if path == root => {
            *post_completion_control = Some(error);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
fn verify_destination_entry_before_source_deletion(
    source_path: &Path,
    destination_path: &Path,
    relative: &Path,
    manifest: &crate::SourceManifest,
    destination_manifest: &crate::DestinationManifest,
    progress: &mut FileOperationProgress<'_>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    // Standard authority uses retained identity and tree-membership evidence
    // on every mount. Strong authority reuses exact version evidence on strict
    // mounts and performs the historical final digest read on reduced mounts.
    let mut verification_interruption = None;
    let verification = destination_manifest.verify_entry_at_with_cancel_counted(
        manifest,
        relative,
        destination_path,
        &mut |verified_path| match progress(source_path, verified_path, 0, false) {
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
    let destination_bytes_rehashed = verification.map_err(|message| FilePickerError::Io {
        op: "revalidate verified destination before source deletion",
        path: destination_path.to_path_buf(),
        message,
    })?;
    io.destination_bytes_hashed = io
        .destination_bytes_hashed
        .saturating_add(destination_bytes_rehashed);
    Ok(())
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

fn committed_operation_unverified(
    source: &Path,
    destination: &Path,
    message: String,
) -> FilePickerError {
    FilePickerError::OperationCommittedButUnverified {
        source: source.to_path_buf(),
        destination: destination.to_path_buf(),
        message,
    }
}

fn destination_committed_move_incomplete(
    source: &Path,
    destination: &Path,
    message: String,
) -> FilePickerError {
    FilePickerError::DestinationCommittedMoveIncomplete {
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

fn verify_case_rename_identity(
    expected: &crate::SourceSnapshot,
    path: &Path,
) -> Result<(), String> {
    let current = crate::snapshot_path(path)
        .map_err(|error| format!("capture renamed object identity at {}: {error}", path.display()))?;
    expected.verify_same_object_after_rename_with_capabilities(
        &current,
        crate::filesystem_capabilities(path),
    )
}

fn execute_picker_case_rename_transaction(
    paths: &[PathBuf],
    transform: impl Fn(&str) -> String,
) -> Result<Vec<PathBuf>, FilePickerError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let parent = paths[0]
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| FilePickerError::InvalidNewItemName(paths[0].display().to_string()))?;
    let mut mappings = Vec::new();
    for source in paths {
        if source.parent() != Some(parent.as_path()) {
            return Err(FilePickerError::Io {
                op: "case rename planning",
                path: source.clone(),
                message: "all selected paths must share one parent directory".to_string(),
            });
        }
        let name = source.file_name().and_then(OsStr::to_str).ok_or_else(|| {
            FilePickerError::InvalidNewItemName(source.display().to_string())
        })?;
        let destination = parent.join(transform(name));
        if destination.as_path() != source.as_path() {
            mappings.push((source.clone(), destination));
        }
    }
    if mappings.is_empty() {
        return Ok(Vec::new());
    }

    let case_key = |path: &Path| path.to_string_lossy().to_lowercase();
    let source_keys = mappings
        .iter()
        .map(|(source, _)| case_key(source))
        .collect::<HashSet<_>>();
    let mut destination_keys = HashSet::new();
    for (_, destination) in &mappings {
        if !destination_keys.insert(case_key(destination)) {
            return Err(FilePickerError::DestinationExists(destination.clone()));
        }
        if destination.exists() && !source_keys.contains(&case_key(destination)) {
            return Err(FilePickerError::DestinationExists(destination.clone()));
        }
    }

    let source_snapshots = mappings
        .iter()
        .map(|(source, _)| {
            crate::snapshot_path(source)
                .map_err(|error| io_error("capture case-rename source identity", source, error))
        })
        .collect::<Result<Vec<_>, _>>()?;

    static NEXT_CASE_RENAME: AtomicU64 = AtomicU64::new(0);
    let workspace = (0..1024)
        .find_map(|_| {
            let sequence = NEXT_CASE_RENAME.fetch_add(1, AtomicOrdering::Relaxed);
            let candidate = parent.join(format!(
                ".tui-file-picker-case-rename-{}-{sequence}",
                std::process::id(),
            ));
            match create_private_directory(&candidate) {
                Ok(()) => Some(Ok(candidate)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()
        .map_err(|error| io_error("create case-rename transaction", &parent, error))?
        .ok_or_else(|| FilePickerError::Io {
            op: "create case-rename transaction",
            path: parent.clone(),
            message: "could not allocate a private transaction workspace".to_string(),
        })?;
    let staging = (0..mappings.len())
        .map(|index| workspace.join(format!("entry-{index:06}")))
        .collect::<Vec<_>>();
    let mut staged = Vec::new();
    let mut installed = Vec::new();

    let operation = (|| -> Result<(), FilePickerError> {
        for (index, (source, _)) in mappings.iter().enumerate() {
            fs::rename(source, &staging[index])
                .map_err(|error| io_error("stage case rename", source, error))?;
            staged.push(index);
            verify_case_rename_identity(&source_snapshots[index], &staging[index])
                .map_err(|message| FilePickerError::Io {
                    op: "verify staged case rename",
                    path: staging[index].clone(),
                    message,
                })?;
        }
        for (index, (_, destination)) in mappings.iter().enumerate() {
            rename_no_replace(&staging[index], destination).map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    FilePickerError::DestinationExists(destination.clone())
                } else {
                    io_error("install case rename", destination, error)
                }
            })?;
            installed.push(index);
            verify_case_rename_identity(&source_snapshots[index], destination)
                .map_err(|message| FilePickerError::Io {
                    op: "verify installed case rename",
                    path: destination.clone(),
                    message,
                })?;
        }
        Ok(())
    })();

    if let Err(error) = operation {
        let mut rollback_errors = Vec::new();
        for &index in installed.iter().rev() {
            let destination = &mappings[index].1;
            if let Err(message) = verify_case_rename_identity(
                &source_snapshots[index],
                destination,
            ) {
                rollback_errors.push(format!(
                    "refused to roll back replaced destination {}: {message}",
                    destination.display(),
                ));
                continue;
            }
            if let Err(rollback) = rename_no_replace(destination, &staging[index]) {
                rollback_errors.push(format!(
                    "restore installed destination {} to staging: {rollback}",
                    destination.display(),
                ));
            }
        }
        for &index in staged.iter().rev() {
            if fs::symlink_metadata(&staging[index]).is_err() {
                continue;
            }
            if let Err(message) = verify_case_rename_identity(
                &source_snapshots[index],
                &staging[index],
            ) {
                rollback_errors.push(format!(
                    "refused to restore changed staged object {}: {message}",
                    staging[index].display(),
                ));
                continue;
            }
            if let Err(rollback) = rename_no_replace(&staging[index], &mappings[index].0) {
                rollback_errors.push(format!(
                    "restore original pathname {}: {rollback}",
                    mappings[index].0.display(),
                ));
            }
        }
        if rollback_errors.is_empty() {
            if let Err(cleanup) = fs::remove_dir(&workspace) {
                rollback_errors.push(format!(
                    "remove empty transaction workspace {}: {cleanup}",
                    workspace.display(),
                ));
            }
        }
        if rollback_errors.is_empty() {
            return Err(error);
        }
        return Err(FilePickerError::Io {
            op: "roll back case-rename transaction",
            path: workspace,
            message: format!(
                "{error}; rollback was incomplete and retained recoverable objects in the transaction workspace: {}",
                rollback_errors.join("; "),
            ),
        });
    }

    // The namespace transaction is already committed. Failure to remove an
    // empty private workspace must not misreport a successful rename as a
    // failed operation (which could prompt an unsafe retry). Best-effort
    // cleanup is safe because every staged object was installed and verified.
    let _ = fs::remove_dir(&workspace);
    let _ = sync_directory(&parent);
    Ok(mappings.into_iter().map(|(_, destination)| destination).collect())
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
fn rename_no_replace_fast(source: &Path, destination: &Path) -> io::Result<()> {
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
fn rename_no_replace_fast(source: &Path, destination: &Path) -> io::Result<()> {
    // std::fs::rename maps to a no-replace move on Windows when the destination
    // already exists.
    fs::rename(source, destination)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn rename_no_replace_fast(source: &Path, destination: &Path) -> io::Result<()> {
    let _ = (source, destination);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace rename is unavailable on this target",
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameNoReplaceMode {
    Atomic,
    /// The filesystem rejected the atomic no-replace primitive. We checked
    /// that the destination was absent immediately before a plain rename.
    /// Another process can still create the destination in that narrow window;
    /// callers surface this honest degraded-mode notice after the commit.
    CheckedBestEffort,
}

impl RenameNoReplaceMode {
    pub fn degraded_warning(self) -> Option<&'static str> {
        matches!(self, Self::CheckedBestEffort).then_some(
            "filesystem lacks atomic no-clobber rename; used a checked best-effort rename",
        )
    }
}

fn checked_best_effort_rename_no_replace(
    source: &Path,
    destination: &Path,
) -> io::Result<RenameNoReplaceMode> {
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("destination already exists: {}", destination.display()),
            ));
        }
        Err(probe) if probe.kind() == io::ErrorKind::NotFound => {}
        Err(probe) => return Err(probe),
    }
    fs::rename(source, destination)?;
    Ok(RenameNoReplaceMode::CheckedBestEffort)
}

#[cfg_attr(not(test), allow(dead_code))] // retained synchronous compatibility/test entry point
fn rename_no_replace_with<F>(
    source: &Path,
    destination: &Path,
    fast_path: F,
) -> io::Result<RenameNoReplaceMode>
where
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    match fast_path(source, destination) {
        Ok(()) => Ok(RenameNoReplaceMode::Atomic),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            checked_best_effort_rename_no_replace(source, destination)
        }
        Err(error) => Err(error),
    }
}

fn rename_no_replace_for_operation(
    source: &Path,
    destination: &Path,
    require_atomic_authority: bool,
) -> io::Result<RenameNoReplaceMode> {
    if require_atomic_authority {
        return crate::rename_path_no_replace(source, destination)
            .map(|()| RenameNoReplaceMode::Atomic);
    }
    rename_no_replace(source, destination)
}

/// Rename without replacing an existing destination, degrading from the atomic
/// no-replace primitive to a checked best-effort rename on mounts that lack it
/// (cifs, ntfs-3g/FUSE). This is the default-path primitive: casual usage must
/// work on real-world mounts; callers surface the degraded-mode warning once.
pub fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<RenameNoReplaceMode> {
    if crate::filesystem_capabilities(destination).atomic_no_replace_rename
        == crate::CapabilitySupport::Unsupported
    {
        return checked_best_effort_rename_no_replace(source, destination);
    }
    match rename_no_replace_fast(source, destination) {
        Ok(()) => {
            crate::record_filesystem_capability(
                destination,
                crate::FilesystemCapabilityKind::AtomicNoReplaceRename,
                crate::CapabilitySupport::Supported,
            );
            Ok(RenameNoReplaceMode::Atomic)
        }
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            crate::record_filesystem_capability(
                destination,
                crate::FilesystemCapabilityKind::AtomicNoReplaceRename,
                crate::CapabilitySupport::Unsupported,
            );
            checked_best_effort_rename_no_replace(source, destination)
        }
        Err(error) => {
            if error.kind() == io::ErrorKind::AlreadyExists {
                crate::record_filesystem_capability(
                    destination,
                    crate::FilesystemCapabilityKind::AtomicNoReplaceRename,
                    crate::CapabilitySupport::Supported,
                );
            }
            Err(error)
        }
    }
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
    let mut io = crate::FileOperationIoCounters::default();
    match safe_copy_path_progress_with_notices_accounted(src, dst, policy, progress, &mut io) {
        Ok(mut outcome) => {
            if let Some(control_error) = outcome.post_publication_control {
                outcome.notices.push(format!(
                    "progress control changed after the verified copy completed: {}",
                    control_error.message()
                ));
            }
            if outcome.notices.is_empty() {
                Ok(())
            } else {
                Err(committed_operation_warning(
                    src,
                    dst,
                    outcome.notices.join("; "),
                ))
            }
        }
        Err(error) => Err(error),
    }
}

#[derive(Debug)]
struct VerifiedCopyOutcome {
    /// The authoritative copy-time source proof. File digests are computed in
    /// the same read that writes private staging; no preliminary hash pass is
    /// performed on the normal copy route.
    source_manifest: crate::SourceManifest,
    /// Identity snapshots for the exact published objects that passed the one
    /// authoritative destination content-verification traversal.
    destination_manifest: crate::DestinationManifest,
    /// Non-fatal metadata, cleanup, or durability limitations observed after
    /// content authority was established.
    notices: Vec<String>,
    /// A control event delivered by the publication-layer `completed == true`
    /// callback after the final destination has been verified. Copy callers
    /// may report this as a completed-copy warning. Move callers must treat it
    /// as incomplete because source cleanup has not begun.
    post_publication_control: Option<FilePickerError>,
}

#[cfg_attr(not(test), allow(dead_code))] // retained synchronous compatibility/test entry point
fn safe_copy_path_progress_with_notices_and_verifier<F>(
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    verify_published: F,
) -> Result<VerifiedCopyOutcome, FilePickerError>
where
    F: FnOnce(&crate::SourceManifest, &Path) -> Result<crate::DestinationManifest, String>,
{
    let mut io = crate::FileOperationIoCounters::default();
    safe_copy_path_progress_with_notices_and_verifier_accounted(
        src,
        dst,
        policy,
        progress,
        &mut io,
        None,
        verify_published,
    )
}

fn safe_copy_path_progress_with_notices_accounted(
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<VerifiedCopyOutcome, FilePickerError> {
    safe_copy_path_progress_with_notices_accounted_with_expected(
        src,
        dst,
        policy,
        progress,
        io,
        None,
    )
}

fn safe_copy_path_progress_with_notices_accounted_with_expected(
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    io: &mut crate::FileOperationIoCounters,
    expected_source: Option<&crate::FileTaskRootProof>,
) -> Result<VerifiedCopyOutcome, FilePickerError> {
    safe_copy_path_progress_with_notices_and_verifier_accounted(
        src,
        dst,
        policy,
        progress,
        io,
        expected_source,
        |manifest, published| manifest.capture_verified_copy_at(published),
    )
}

fn safe_copy_path_progress_with_notices_and_verifier_accounted<F>(
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    progress: &mut FileOperationProgress<'_>,
    io: &mut crate::FileOperationIoCounters,
    expected_source: Option<&crate::FileTaskRootProof>,
    verify_published: F,
) -> Result<VerifiedCopyOutcome, FilePickerError>
where
    F: FnOnce(&crate::SourceManifest, &Path) -> Result<crate::DestinationManifest, String>,
{
    let parent = dst.parent().unwrap_or_else(|| Path::new("."));
    if fs::symlink_metadata(dst).is_ok() {
        return Err(FilePickerError::DestinationExists(dst.to_path_buf()));
    }
    let staging_container = create_unique_staging_directory(parent)?;
    let staging = staging_container.join("payload");
    let mut visited = HashSet::new();
    let mut metadata_warnings = Vec::new();
    let mut source_manifest = crate::SourceManifest::new(policy.verification);
    let mut staged_destination_manifest = crate::DestinationManifest::new(policy.verification);
    io.source_tree_walks = io.source_tree_walks.saturating_add(1);
    let result = copy_path_to_staging(
        src,
        src,
        &staging,
        policy,
        &mut visited,
        progress,
        &mut metadata_warnings,
        &mut source_manifest,
        &mut staged_destination_manifest,
        io,
    )
    .and_then(|()| {
        if let Some(expected) = expected_source {
            expected
                .destination_manifest
                .verify_captured_replay_source(
                    &expected.source_manifest,
                    &source_manifest,
                    crate::filesystem_capabilities(src),
                )
                .map_err(|message| FilePickerError::Io {
                    op: "verify replay source authority",
                    path: src.to_path_buf(),
                    message,
                })?;
        }
        progress(src, dst, 0, false)?;
        io.rename_attempts = io.rename_attempts.saturating_add(1);
        let publication_mode = rename_no_replace_for_operation(
            &staging,
            dst,
            expected_source.is_some(),
        )
        .map_err(|err| {
            if err.kind() == io::ErrorKind::AlreadyExists {
                FilePickerError::DestinationExists(dst.to_path_buf())
            } else {
                io_error("commit staged copy", dst, err)
            }
        })?;
        if matches!(publication_mode, RenameNoReplaceMode::CheckedBestEffort) {
            io.rename_fallbacks = io.rename_fallbacks.saturating_add(1);
        }
        if policy.verbose_degrade_notices {
            if let Some(warning) = publication_mode.degraded_warning() {
                metadata_warnings.push(warning.to_string());
            }
        }
        let destination_manifest = if policy.verification == VerificationMode::Strong {
            io.destination_tree_walks = io.destination_tree_walks.saturating_add(1);
            io.destination_bytes_hashed = io
                .destination_bytes_hashed
                .saturating_add(source_manifest.total_file_bytes());
            verify_published(&source_manifest, dst).map_err(|message| {
                committed_operation_unverified(
                    src,
                    dst,
                    format!(
                        "post-publication content verification failed; source retained: {message}"
                    ),
                )
            })?
        } else {
            let published_root = crate::snapshot_path(dst)
                .map_err(|error| io_error("identify published copy root", dst, error))?;
            staged_destination_manifest
                .identity_after_root_rename(
                    published_root,
                    crate::filesystem_capabilities(dst),
                )
                .map_err(|message| {
                    committed_operation_unverified(
                        src,
                        dst,
                        format!(
                            "post-publication identity verification failed; source retained: {message}"
                        ),
                    )
                })?
        };
        Ok(destination_manifest)
    });
    match result {
        Ok(destination_manifest) => {
            let mut notices = metadata_warnings;
            if let Err(err) = fs::remove_dir(&staging_container) {
                if err.kind() != io::ErrorKind::NotFound {
                    notices.push(format!(
                        "could not remove empty staging directory {}: {err}",
                        staging_container.display()
                    ));
                }
            }
            if policy.verification == VerificationMode::Standard {
                if let Err(error) = sync_published_root_standard(dst, io) {
                    notices.push(error.message().to_string());
                }
            }
            if let Err(err) = sync_directory_accounted(parent, io) {
                notices.push(format!(
                    "destination directory synchronization failed after verified publication: {err}"
                ));
            }
            let post_publication_control = progress(src, dst, 0, true).err();
            Ok(VerifiedCopyOutcome {
                source_manifest,
                destination_manifest,
                notices,
                post_publication_control,
            })
        }
        Err(err) => {
            cleanup_staging(&staging_container);
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_path_to_staging(
    root_src: &Path,
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    visited: &mut HashSet<PathBuf>,
    progress: &mut FileOperationProgress<'_>,
    metadata_warnings: &mut Vec<String>,
    source_manifest: &mut crate::SourceManifest,
    destination_manifest: &mut crate::DestinationManifest,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    progress(src, dst, 0, false)?;
    let source_snapshot = crate::snapshot_path(src)
        .map_err(|err| io_error("capture source identity", src, err))?;
    let relative = src.strip_prefix(root_src).map_err(|_| FilePickerError::Io {
        op: "build copy-time source manifest",
        path: src.to_path_buf(),
        message: "source traversal escaped its root".to_string(),
    })?.to_path_buf();
    let symlink_metadata = fs::symlink_metadata(src)
        .map_err(|err| io_error("read metadata", src, err))?;
    if symlink_metadata.file_type().is_symlink() {
        match policy.symlink_copy {
            SymlinkCopyPolicy::Reject => {
                return Err(FilePickerError::SymlinkRejected(src.to_path_buf()))
            }
            SymlinkCopyPolicy::FollowTarget => {}
        }
    }
    let metadata = fs::metadata(src).map_err(|err| io_error("read target metadata", src, err))?;
    if metadata.is_dir() {
        let src_canon = src
            .canonicalize()
            .map_err(|err| io_error("canonicalize source", src, err))?;
        let dst_parent_canon = dst
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()
            .map_err(|err| io_error("canonicalize destination parent", dst, err))?;
        if dst_parent_canon.starts_with(&src_canon) || !visited.insert(src_canon.clone()) {
            return Err(FilePickerError::CopyCycleRejected {
                source: src_canon,
                destination: dst.to_path_buf(),
            });
        }
        source_manifest
            .insert(relative.clone(), source_snapshot.clone(), None)
            .map_err(|message| FilePickerError::Io {
                op: "record copy-time directory proof",
                path: src.to_path_buf(),
                message,
            })?;
        fs::create_dir(dst).map_err(|err| io_error("create staged directory", dst, err))?;
        let result = copy_dir_contents(
            root_src,
            src,
            dst,
            policy,
            visited,
            progress,
            metadata_warnings,
            source_manifest,
            destination_manifest,
            io,
        )
        .and_then(|()| {
            match crate::verify_path_with_capabilities(
                src,
                &source_snapshot,
                crate::filesystem_capabilities(src),
            ) {
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
            if policy.verification == VerificationMode::Strong {
                if let Err(err) = sync_directory_accounted(dst, io) {
                    if crate::filesystem_identity_policy(dst)
                        == crate::FilesystemIdentityPolicy::ContentVerifiedPortable
                    {
                        metadata_warnings.push(format!(
                            "{}: directory synchronization is unavailable on this filesystem: {err}",
                            dst.display()
                        ));
                    } else {
                        return Err(io_error("sync copied directory", dst, err));
                    }
                }
            }
            let destination_snapshot = crate::snapshot_path(dst)
                .map_err(|err| io_error("capture staged directory identity", dst, err))?;
            destination_manifest
                .insert(relative.clone(), destination_snapshot)
                .map_err(|message| FilePickerError::Io {
                    op: "record staged directory identity",
                    path: dst.to_path_buf(),
                    message,
                })?;
            Ok(())
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
        let (proof_snapshot, digest, destination_snapshot) = copy_regular_file(
            src,
            dst,
            &metadata,
            expected_snapshot,
            policy.verification,
            src == root_src,
            progress,
            metadata_warnings,
            io,
        )?;
        source_manifest
            .insert(relative.clone(), proof_snapshot, digest)
            .map_err(|message| FilePickerError::Io {
                op: "record copy-time file proof",
                path: src.to_path_buf(),
                message,
            })?;
        destination_manifest
            .insert(relative, destination_snapshot)
            .map_err(|message| FilePickerError::Io {
                op: "record staged file identity",
                path: dst.to_path_buf(),
                message,
            })?;
        progress(src, dst, 0, true)
    } else {
        Err(FilePickerError::Io {
            op: "copy special file",
            path: src.to_path_buf(),
            message: "special files are not supported".to_string(),
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_dir_contents(
    root_src: &Path,
    src: &Path,
    dst: &Path,
    policy: FileOperationPolicy,
    visited: &mut HashSet<PathBuf>,
    progress: &mut FileOperationProgress<'_>,
    metadata_warnings: &mut Vec<String>,
    source_manifest: &mut crate::SourceManifest,
    destination_manifest: &mut crate::DestinationManifest,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    let mut entries = fs::read_dir(src)
        .map_err(|err| io_error("read directory", src, err))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| io_error("read directory entry", src, err))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        progress(src, dst, 0, false)?;
        let child_src = entry.path();
        let child_dst = dst.join(entry.file_name());
        copy_path_to_staging(
            root_src,
            &child_src,
            &child_dst,
            policy,
            visited,
            progress,
            metadata_warnings,
            source_manifest,
            destination_manifest,
            io,
        )?;
    }
    Ok(())
}

fn copy_regular_file(
    src: &Path,
    dst: &Path,
    _metadata: &fs::Metadata,
    expected_snapshot: Option<&crate::SourceSnapshot>,
    verification: VerificationMode,
    sync_standard_root_file: bool,
    progress: &mut FileOperationProgress<'_>,
    metadata_warnings: &mut Vec<String>,
    io: &mut crate::FileOperationIoCounters,
) -> Result<
    (
        crate::SourceSnapshot,
        Option<crate::ContentDigest>,
        crate::SourceSnapshot,
    ),
    FilePickerError,
> {
    let mut source = fs::File::open(src).map_err(|err| io_error("open source file", src, err))?;
    let opened_snapshot = crate::snapshot_open_file(&source)
        .map_err(|err| io_error("capture opened source identity", src, err))?;
    if let Some(expected) = expected_snapshot {
        expected
            .verify_same_object_and_version_with_capabilities(
                &opened_snapshot,
                crate::filesystem_capabilities(src),
            )
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
    let mut sha = (verification == VerificationMode::Strong).then(crate::Sha256::new);
    loop {
        progress(src, dst, 0, false)?;
        let read = source
            .read(&mut buffer)
            .map_err(|err| io_error("read source file", src, err))?;
        if read == 0 {
            break;
        }
        destination
            .write_all(&buffer[..read])
            .map_err(|err| io_error("write staged file", dst, err))?;
        if let Some(sha) = sha.as_mut() {
            sha.update(&buffer[..read]);
        }
        copied = copied.saturating_add(read as u64);
        io.bytes_copied = io.bytes_copied.saturating_add(read as u64);
        if verification == VerificationMode::Strong {
            io.source_bytes_hashed = io.source_bytes_hashed.saturating_add(read as u64);
        }
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
    metadata_warnings.extend(
        crate::preserve_open_file_metadata(&source, &destination)
            .into_iter()
            .map(|warning| format!("{}: {warning}", src.display())),
    );
    if verification == VerificationMode::Strong || sync_standard_root_file {
        io.file_sync_calls = io.file_sync_calls.saturating_add(1);
        destination
            .sync_all()
            .map_err(|err| io_error("sync staged file and metadata", dst, err))?;
    }
    let destination_snapshot = crate::snapshot_open_file(&destination)
        .map_err(|err| io_error("capture staged file identity", dst, err))?;
    let digest = sha.map(crate::Sha256::finalize);
    Ok((opened_snapshot, digest, destination_snapshot))
}

fn sync_published_root_standard(
    path: &Path,
    io: &mut crate::FileOperationIoCounters,
) -> Result<(), FilePickerError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_error("inspect published root for synchronization", path, error))?;
    if metadata.file_type().is_dir() {
        sync_directory_accounted(path, io)
            .map_err(|error| io_error("sync published root directory", path, error))?;
    } else if !metadata.file_type().is_file() {
        return Err(FilePickerError::Io {
            op: "sync published root",
            path: path.to_path_buf(),
            message: "published root is neither a regular file nor a directory".to_string(),
        });
    }
    Ok(())
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

fn sync_directory_accounted(
    path: &Path,
    io: &mut crate::FileOperationIoCounters,
) -> io::Result<()> {
    io.directory_sync_calls = io.directory_sync_calls.saturating_add(1);
    sync_directory(path)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        if crate::filesystem_capabilities(path).directory_sync
            == crate::CapabilitySupport::Unsupported
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "directory synchronization is unsupported on this mount",
            ));
        }
        let result = fs::File::open(path).and_then(|directory| directory.sync_all());
        let support = match &result {
            Ok(()) => crate::CapabilitySupport::Supported,
            Err(error)
                if error.kind() == io::ErrorKind::Unsupported
                    || matches!(
                        error.raw_os_error(),
                        Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)
                    ) =>
            {
                crate::CapabilitySupport::Unsupported
            }
            Err(_) => crate::CapabilitySupport::Unknown,
        };
        crate::record_filesystem_capability(
            path,
            crate::FilesystemCapabilityKind::DirectorySync,
            support,
        );
        result
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
    fn standalone_title_case_matches_ampersand_and_parenthetical_contract() {
        assert_eq!(
            default_picker_title_case("Booker T & the MG's"),
            "Booker T & The MG's",
        );
        assert_eq!(
            default_picker_title_case("Neil Young & the Shocking Pinks"),
            "Neil Young & The Shocking Pinks",
        );
        assert_eq!(
            default_picker_title_case("(Japan P-11356 Promo LP / 32-192)"),
            "(Japan P-11356 Promo LP / 32-192)",
        );
        assert_eq!(default_picker_title_case("TELL US WHY"), "Tell Us Why");
        assert_eq!(default_picker_title_case("(US PROMO LP)"), "(US Promo LP)");
        assert_eq!(
            default_picker_title_case("KOOL & THE GANG, EMERGENCY, 1984"),
            "Kool & The Gang, Emergency, 1984"
        );
        assert_eq!(
            default_picker_title_case("Kool &The Gang, Emergency, 1984"),
            "Kool &The Gang, Emergency, 1984"
        );
        assert_eq!(
            default_picker_title_case("Kool&The Gang, Emergency, 1984"),
            "Kool&The Gang, Emergency, 1984"
        );
        assert_eq!(
            default_picker_title_case("Jack and the Beanstalk"),
            "Jack and the Beanstalk"
        );
    }

    #[test]
    fn filesystem_cut_and_copy_publish_the_same_ordered_host_text_projection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("track 01.flac");
        std::fs::write(&file, b"audio").expect("fixture");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == file)
            .expect("fixture entry");
        picker.set_file_cursor(index, 10);

        let published = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let sink = std::rc::Rc::clone(&published);
        let expected = file.to_string_lossy().to_string();
        crate::text_input::with_scoped_shared_text_clipboard("prior text", || {
            crate::text_input::with_scoped_shared_text_clipboard_publish_hook(
                move |text| sink.borrow_mut().push(text.to_string()),
                || {
                    picker.try_copy_current().expect("copy");
                    picker.try_cut_current().expect("cut");
                },
            );

            assert_eq!(crate::text_input::read_shared_text_clipboard(), "prior text");
            let clipboard = picker.clipboard.as_ref().expect("structured clipboard");
            assert_eq!(clipboard.paths(), &[file.clone()]);
        });
        assert_eq!(published.borrow().as_slice(), &[expected.clone(), expected]);
    }

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
    fn exact_paste_plan_preserves_destinations_and_preflights_the_whole_batch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        let first_source = source_dir.join("first.flac");
        let second_source = source_dir.join("second.flac");
        fs::write(&first_source, b"first").expect("first source");
        fs::write(&second_source, b"second").expect("second source");

        let exact_destination = destination_dir.join("kept-name.flac");
        let success = execute_exact_paste_plan(
            &PastePlan {
                mode: FilePickerClipboardMode::Copy,
                mappings: vec![PasteMapping {
                    source: first_source.clone(),
                    destination: exact_destination.clone(),
                }],
            },
            FileOperationPolicy::default(),
        )
        .expect("exact copy");
        assert_eq!(success.mappings[0].destination, exact_destination);
        assert_eq!(fs::read(&exact_destination).expect("copied bytes"), b"first");

        let blocked_destination = destination_dir.join("blocked.flac");
        fs::write(&blocked_destination, b"existing").expect("blocker");
        let never_created = destination_dir.join("never-created.flac");
        let failure = execute_exact_paste_plan(
            &PastePlan {
                mode: FilePickerClipboardMode::Copy,
                mappings: vec![
                    PasteMapping {
                        source: second_source.clone(),
                        destination: never_created.clone(),
                    },
                    PasteMapping {
                        source: first_source,
                        destination: blocked_destination.clone(),
                    },
                ],
            },
            FileOperationPolicy::default(),
        )
        .expect_err("destination conflict must fail preflight");
        assert!(matches!(failure.error, FilePickerError::DestinationExists(path) if path == blocked_destination));
        assert!(!never_created.exists(), "preflight must prevent partial mutation");
        assert_eq!(fs::read(blocked_destination).expect("blocker intact"), b"existing");
    }

    #[test]
    fn configured_sort_is_applied_and_same_column_click_toggles_direction() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("a.flac"), b"longer").expect("a");
        fs::write(temp.path().join("b.flac"), b"x").expect("b");

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            sort_key: FilePickerSortKey::Size,
            sort_reverse: false,
            ..FilePickerConfig::default()
        });
        let names = picker.entries().iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["b.flac", "a.flac"]);
        assert_eq!(picker.sort_key(), FilePickerSortKey::Size);
        assert!(!picker.sort_reverse());
        assert!(!picker.sort_changed());

        picker.set_sort(FilePickerSortKey::Size);
        let names = picker.entries().iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["a.flac", "b.flac"]);
        assert!(picker.sort_reverse());
        assert!(picker.sort_changed());

        picker.set_sort(FilePickerSortKey::Name);
        let names = picker.entries().iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["a.flac", "b.flac"]);
        assert_eq!(picker.sort_key(), FilePickerSortKey::Name);
        assert!(!picker.sort_reverse(), "a newly selected column starts ascending");
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

    #[test]
    fn post_publication_verification_failure_is_a_failed_committed_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&source, b"source").expect("source");
        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());

        // The injected post-publication verifier only runs under Strong;
        // Standard skips destination re-verification by design.
        let policy = FileOperationPolicy {
            verification: VerificationMode::Strong,
            ..FileOperationPolicy::default()
        };
        let error = safe_copy_path_progress_with_notices_and_verifier(
            &source,
            &destination,
            policy,
            &mut progress,
            |_manifest, _published| Err("fixture verification failure".to_string()),
        )
        .expect_err("published but unverified destination must fail");

        assert!(matches!(
            error,
            FilePickerError::OperationCommittedButUnverified { .. }
        ));
        assert_eq!(fs::read(&source).expect("source retained"), b"source");
        assert_eq!(
            fs::read(&destination).expect("destination preserved for inspection"),
            b"source"
        );
    }

    #[test]
    fn only_verified_committed_warnings_are_classified_as_completed() {
        let source = PathBuf::from("source");
        let destination = PathBuf::from("destination");
        assert_eq!(
            classify_paste_root_result(Err(committed_operation_warning(
                &source,
                &destination,
                "verified copy; directory sync unavailable".to_string(),
            )))
            .expect("verified warning remains completed"),
            Some("verified copy; directory sync unavailable".to_string())
        );
        assert!(matches!(
            classify_paste_root_result(Err(committed_operation_unverified(
                &source,
                &destination,
                "content verification failed".to_string(),
            ))),
            Err(FilePickerError::OperationCommittedButUnverified { .. })
        ));
        assert!(matches!(
            classify_paste_root_result(Err(destination_committed_move_incomplete(
                &source,
                &destination,
                "source cleanup incomplete".to_string(),
            ))),
            Err(FilePickerError::DestinationCommittedMoveIncomplete { .. })
        ));
    }

    #[test]
    fn final_control_after_verified_copy_is_a_completed_copy_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&source, b"source").expect("source");

        let mut completed_callbacks = 0usize;
        let mut staging_completion_seen = false;
        let mut publication_seen = false;
        let mut progress =
            |_source: &Path, callback_destination: &Path, _bytes: u64, completed: bool| {
                if !completed {
                    return Ok(());
                }
                completed_callbacks += 1;
                if callback_destination == destination.as_path() && destination.exists() {
                    publication_seen = true;
                    Err(FilePickerError::OperationSkipped)
                } else {
                    staging_completion_seen = true;
                    Ok(())
                }
            };
        let result = safe_copy_path_progress(
            &source,
            &destination,
            FileOperationPolicy::default(),
            &mut progress,
        );

        assert!(staging_completion_seen, "staging must complete before publication");
        assert!(publication_seen, "control must be injected after publication");
        assert_eq!(completed_callbacks, 2);
        assert!(matches!(
            classify_paste_root_result(result),
            Ok(Some(message)) if message.contains("verified copy completed")
        ));
        assert_eq!(fs::read(&source).expect("source retained"), b"source");
        assert_eq!(
            fs::read(&destination).expect("verified copy retained"),
            b"source"
        );
    }

    fn assert_one_root_cut_stopped_after_publication_is_incomplete(
        control_error: FilePickerError,
        expected_reason: &str,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        let source = source_dir.join("track.flac");
        let destination = destination_dir.join("track.flac");
        fs::write(&source, b"audio").expect("source");

        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let plan = plan_filesystem_paste(&clipboard, &destination_dir).expect("paste plan");
        assert_eq!(plan.mappings.len(), 1);

        let mut completed_callbacks = 0usize;
        let mut staging_completion_seen = false;
        let mut publication_seen = false;
        let mut progress =
            |_source: &Path, callback_destination: &Path, _bytes: u64, completed: bool| {
                if !completed {
                    return Ok(());
                }
                completed_callbacks += 1;
                if callback_destination == destination.as_path() && destination.exists() {
                    publication_seen = true;
                    Err(control_error.clone())
                } else {
                    staging_completion_seen = true;
                    Ok(())
                }
            };
        let failure = execute_paste_plan_progress_with(&plan, |mode, mapping| {
            assert_eq!(mode, FilePickerClipboardMode::Cut);
            copy_then_delete_progress(
                &mapping.source,
                &mapping.destination,
                FileOperationPolicy::default(),
                &mut progress,
            )
        })
        .expect_err("a stopped post-publication move must not return PasteSuccess");

        assert!(staging_completion_seen, "staging completion must precede publication");
        assert!(publication_seen, "the destination must exist before control is injected");
        assert_eq!(
            completed_callbacks, 2,
            "the injected control event must occur on the final verified-copy callback"
        );
        assert!(failure.completed.is_empty());
        assert_eq!(failure.remaining_sources, vec![source.clone()]);
        match &failure.error {
            FilePickerError::DestinationCommittedMoveIncomplete {
                source: failed_source,
                destination: failed_destination,
                message,
            } => {
                assert_eq!(failed_source, &source);
                assert_eq!(failed_destination, &destination);
                assert!(message.contains(expected_reason), "message={message}");
                assert!(
                    message.contains("source cleanup did not begin"),
                    "message={message}"
                );
            }
            other => panic!("unexpected result: {other:?}"),
        }
        assert_eq!(fs::read(&source).expect("source retained"), b"audio");
        assert_eq!(
            fs::read(&destination).expect("verified destination retained"),
            b"audio"
        );

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: source_dir.clone(),
            ..FilePickerConfig::default()
        });
        picker.clipboard = Some(clipboard.clone());
        picker.paste_task = Some(PickerPasteTask {
            progress: crate::FileTaskProgressState::new(
                crate::FileTaskKind::Move,
                "Moving files",
                picker.theme.clone(),
            ),
            receiver: None,
            control: Arc::new(AtomicU8::new(PASTE_CONTROL_RUNNING)),
            clipboard,
            target_dir: destination_dir,
            plan: plan.clone(),
            retry_plan: None,
        });
        picker.finish_picker_paste(Err(failure));

        assert_eq!(
            picker
                .clipboard
                .as_ref()
                .expect("cut retry state retained")
                .paths(),
            &[source.clone()]
        );
        assert_eq!(
            picker
                .paste_retry_plan
                .as_ref()
                .expect("exact retry plan retained")
                .mappings,
            vec![PasteMapping {
                source: source.clone(),
                destination: destination.clone(),
            }]
        );
        assert_eq!(picker.current_dir(), source_dir.as_path());
        let task = picker.paste_task.as_ref().expect("failed progress retained");
        assert!(matches!(task.progress.phase, crate::FileTaskPhase::Failed));
    }

    #[test]
    fn final_skip_after_verified_publication_keeps_one_root_cut_incomplete() {
        assert_one_root_cut_stopped_after_publication_is_incomplete(
            FilePickerError::OperationSkipped,
            "user skipped the move",
        );
    }

    #[test]
    fn final_abort_after_verified_publication_keeps_one_root_cut_incomplete() {
        assert_one_root_cut_stopped_after_publication_is_incomplete(
            FilePickerError::OperationCancelled,
            "user aborted the move",
        );
    }

    fn assert_control_after_complete_source_deletion_is_a_completed_move(
        control_error: FilePickerError,
        expected_text: &str,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("destination.flac");
        fs::write(&source, b"audio").expect("source");

        let mut root_delete_callback_seen = false;
        let mut progress =
            |callback_source: &Path, callback_destination: &Path, _bytes: u64, completed: bool| {
                if completed
                    && callback_destination == destination.as_path()
                    && !callback_source.exists()
                {
                    root_delete_callback_seen = true;
                    Err(control_error.clone())
                } else {
                    Ok(())
                }
            };
        let plan = PastePlan {
            mode: FilePickerClipboardMode::Cut,
            mappings: vec![PasteMapping {
                source: source.clone(),
                destination: destination.clone(),
            }],
        };
        let success = execute_paste_plan_progress_with(&plan, |mode, mapping| {
            assert_eq!(mode, FilePickerClipboardMode::Cut);
            copy_then_delete_progress(
                &mapping.source,
                &mapping.destination,
                FileOperationPolicy::default(),
                &mut progress,
            )
        })
        .expect("a control event after root deletion must remain a completed move");

        assert!(
            root_delete_callback_seen,
            "control must be injected only after the source root was removed"
        );
        assert_eq!(&success.mappings, &plan.mappings);
        assert!(success.warnings.iter().any(|warning| {
            warning.message.contains(expected_text)
                && warning.message.contains("move completed")
        }));
        assert!(!source.exists(), "completed move source must remain deleted");
        assert_eq!(
            fs::read(&destination).expect("destination retained"),
            b"audio"
        );
    }

    #[test]
    fn skip_after_complete_source_deletion_is_a_completed_move_warning() {
        assert_control_after_complete_source_deletion_is_a_completed_move(
            FilePickerError::OperationSkipped,
            "skip request arrived after",
        );
    }

    #[test]
    fn abort_after_complete_source_deletion_is_a_completed_move_warning() {
        assert_control_after_complete_source_deletion_is_a_completed_move(
            FilePickerError::OperationCancelled,
            "abort request arrived after",
        );
    }

    #[test]
    fn control_before_source_deletion_restores_the_original_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("destination.flac");
        fs::write(&source, b"audio").expect("source");

        let mut post_quarantine_control_seen = false;
        let mut progress =
            |callback_source: &Path, callback_destination: &Path, _bytes: u64, completed: bool| {
                if !completed
                    && callback_destination == destination.as_path()
                    && destination.exists()
                    && !source.exists()
                    && callback_source.file_name() == Some(OsStr::new("payload"))
                {
                    post_quarantine_control_seen = true;
                    Err(FilePickerError::OperationSkipped)
                } else {
                    Ok(())
                }
            };
        let error = copy_then_delete_progress(
            &source,
            &destination,
            FileOperationPolicy::default(),
            &mut progress,
        )
        .expect_err("control before deletion must stop the move");

        assert!(
            post_quarantine_control_seen,
            "control must be injected after quarantine but before source deletion"
        );
        assert!(matches!(
            error,
            FilePickerError::DestinationCommittedMoveIncomplete { message, .. }
                if message.contains("cleanup stopped before any quarantined entry was deleted")
                    && message.contains("source restored to")
        ));
        assert_eq!(fs::read(&source).expect("source restored"), b"audio");
        assert_eq!(
            fs::read(&destination).expect("verified destination retained"),
            b"audio"
        );
        assert!(
            fs::read_dir(temp.path())
                .expect("temp entries")
                .all(|entry| !entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tui-file-picker-source-quarantine-")),
            "restoration must remove the empty quarantine container"
        );
    }

    #[test]
    fn control_after_child_deletion_is_an_incomplete_recursive_move() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let source_nested = source.join("nested");
        let destination = temp.path().join("destination");
        fs::create_dir_all(&source_nested).expect("source directory");
        fs::write(source.join("a.flac"), b"a").expect("a");
        fs::write(source_nested.join("b.flac"), b"b").expect("b");
        let policy = FileOperationPolicy {
            delete: DeletePolicy::Recursive,
            ..FileOperationPolicy::default()
        };
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let plan = PastePlan {
            mode: FilePickerClipboardMode::Cut,
            mappings: vec![PasteMapping {
                source: source.clone(),
                destination: destination.clone(),
            }],
        };
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: source_nested.clone(),
            ..FilePickerConfig::default()
        });

        let mut child_delete_callback_seen = false;
        let mut progress =
            |callback_source: &Path, _destination: &Path, _bytes: u64, completed: bool| {
                if completed
                    && callback_source.file_name() == Some(OsStr::new("a.flac"))
                    && !callback_source.exists()
                {
                    child_delete_callback_seen = true;
                    Err(FilePickerError::OperationSkipped)
                } else {
                    Ok(())
                }
            };
        let failure = execute_paste_plan_progress_with(&plan, |mode, mapping| {
            assert_eq!(mode, FilePickerClipboardMode::Cut);
            copy_then_delete_progress(
                &mapping.source,
                &mapping.destination,
                policy,
                &mut progress,
            )
        })
        .expect_err("a stopped recursive cleanup must remain incomplete");

        assert!(child_delete_callback_seen);
        assert!(matches!(
            &failure.error,
            FilePickerError::DestinationCommittedMoveIncomplete { message, .. }
                if message.contains("after deleting 1 quarantined entry")
                    && message.contains("undeleted remnants remain")
        ));
        assert!(!source.exists(), "partially deleted source remains quarantined");
        assert_eq!(fs::read(destination.join("a.flac")).expect("destination a"), b"a");
        assert_eq!(
            fs::read(destination.join("nested/b.flac")).expect("destination b"),
            b"b"
        );

        picker.clipboard = Some(clipboard.clone());
        picker.paste_task = Some(PickerPasteTask {
            progress: crate::FileTaskProgressState::new(
                crate::FileTaskKind::Move,
                "Moving files",
                picker.theme.clone(),
            ),
            receiver: None,
            control: Arc::new(AtomicU8::new(PASTE_CONTROL_RUNNING)),
            clipboard,
            target_dir: temp.path().to_path_buf(),
            plan,
            retry_plan: None,
        });
        picker.finish_picker_paste(Err(failure));

        let expected_current = destination.join("nested");
        assert_eq!(
            picker.current_dir(),
            expected_current.as_path(),
            "a current directory under an irreversibly removed source must adopt the verified destination"
        );
        assert!(picker.clipboard.is_none(), "partial deletion has no complete retry source");
        assert!(picker.paste_retry_plan.is_none());
        assert!(matches!(
            picker
                .paste_task
                .as_ref()
                .expect("failed task retained")
                .progress
                .phase,
            crate::FileTaskPhase::Failed
        ));
    }

    #[test]
    fn default_policy_recursively_cleans_verified_directory_move_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_parent = temp.path().join("source-parent");
        let destination_parent = temp.path().join("destination-parent");
        let source = source_parent.join("album");
        let destination = destination_parent.join("album");
        fs::create_dir_all(source.join("disc/notes")).expect("source tree");
        fs::create_dir(&destination_parent).expect("destination parent");
        fs::write(source.join("disc/track.flac"), b"audio").expect("track");
        fs::write(source.join("disc/notes/readme.txt"), b"notes").expect("notes");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");

        let policy = FileOperationPolicy::default();
        assert_eq!(policy.delete, DeletePolicy::FilesAndEmptyDirectories);
        TEST_FORCE_COPY_THEN_DELETE_MOVE.with(|flag| flag.set(true));
        let success = paste_filesystem_clipboard(&clipboard, &destination_parent, policy)
            .expect("default-policy portable directory move must complete");

        assert_eq!(
            success.mappings,
            vec![PasteMapping {
                source: source.clone(),
                destination: destination.clone(),
            }]
        );
        assert!(!source.exists(), "verified move cleanup must remove the source tree");
        assert_eq!(
            fs::read(destination.join("disc/track.flac")).expect("destination track"),
            b"audio"
        );
        assert_eq!(
            fs::read(destination.join("disc/notes/readme.txt")).expect("destination notes"),
            b"notes"
        );
    }

    #[test]
    fn default_explicit_delete_policy_still_refuses_nonempty_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = temp.path().join("album");
        fs::create_dir(&directory).expect("directory");
        fs::write(directory.join("track.flac"), b"audio").expect("track");

        let error = delete_path(&directory, DeletePolicy::FilesAndEmptyDirectories)
            .expect_err("explicit default delete must not recurse");

        assert!(directory.exists());
        assert_eq!(fs::read(directory.join("track.flac")).expect("track"), b"audio");
        assert!(matches!(error, FilePickerError::Io { .. }));
    }

    #[test]
    fn retained_cut_retry_reuses_original_destination_without_duplicate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        let source = source_dir.join("track.flac");
        let destination = destination_dir.join("track.flac");
        fs::write(&source, b"audio").expect("source");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let initial_plan = plan_filesystem_paste(&clipboard, &destination_dir).expect("plan");

        let mut publication_seen = false;
        let mut first_progress =
            |_source: &Path, callback_destination: &Path, _bytes: u64, completed: bool| {
                if completed
                    && callback_destination == destination.as_path()
                    && destination.exists()
                {
                    publication_seen = true;
                    Err(FilePickerError::OperationSkipped)
                } else {
                    Ok(())
                }
            };
        let first_failure = execute_paste_plan_progress_with(&initial_plan, |mode, mapping| {
            assert_eq!(mode, FilePickerClipboardMode::Cut);
            copy_then_delete_progress(
                &mapping.source,
                &mapping.destination,
                FileOperationPolicy::default(),
                &mut first_progress,
            )
        })
        .expect_err("first move must stop after verified publication");
        assert!(publication_seen);

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: source_dir.clone(),
            ..FilePickerConfig::default()
        });
        picker.clipboard = Some(clipboard.clone());
        picker.paste_task = Some(PickerPasteTask {
            progress: crate::FileTaskProgressState::new(
                crate::FileTaskKind::Move,
                "Moving files",
                picker.theme.clone(),
            ),
            receiver: None,
            control: Arc::new(AtomicU8::new(PASTE_CONTROL_RUNNING)),
            clipboard: clipboard.clone(),
            target_dir: destination_dir.clone(),
            plan: initial_plan.clone(),
            retry_plan: None,
        });
        picker.finish_picker_paste(Err(first_failure));

        let (retry_plan, resume_existing) = plan_filesystem_paste_with_retry(
            picker.clipboard.as_ref().expect("cut clipboard retained"),
            &destination_dir,
            picker.paste_retry_plan.as_ref(),
        )
        .expect("retry plan");
        assert!(resume_existing);
        assert_eq!(&retry_plan.mappings, &initial_plan.mappings);
        assert_eq!(
            retry_plan.mappings[0].destination.as_path(),
            destination.as_path()
        );

        let retained_retry = picker
            .paste_retry_plan
            .clone()
            .expect("retained retry evidence");
        let mut retry_progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
        let retry_success = execute_paste_plan_progress_with_resume(
            &retry_plan,
            FileOperationPolicy::default(),
            Some(&retained_retry),
            &mut retry_progress,
        )
        .expect("retry must verify the existing destination and finish cleanup");
        assert_eq!(&retry_success.mappings, &retry_plan.mappings);
        assert!(!source.exists());
        assert_eq!(fs::read(&destination).expect("single destination"), b"audio");
        assert!(!destination_dir.join("track 2.flac").exists());
        let destination_entries = fs::read_dir(&destination_dir)
            .expect("read destination")
            .collect::<Result<Vec<_>, _>>()
            .expect("destination entries");
        assert_eq!(destination_entries.len(), 1, "retry must not create a duplicate");

        let retry_clipboard = picker.clipboard.clone().expect("retry clipboard");
        picker.paste_task = Some(PickerPasteTask {
            progress: crate::FileTaskProgressState::new(
                crate::FileTaskKind::Move,
                "Moving files",
                picker.theme.clone(),
            ),
            receiver: None,
            control: Arc::new(AtomicU8::new(PASTE_CONTROL_RUNNING)),
            clipboard: retry_clipboard,
            target_dir: destination_dir,
            plan: retry_plan,
            retry_plan: None,
        });
        picker.finish_picker_paste(Ok(retry_success));
        assert!(picker.clipboard.is_none(), "completed cut retry clears clipboard");
        assert!(picker.paste_retry_plan.is_none(), "completed retry clears exact plan");
    }

    #[test]
    fn public_retry_api_reuses_exact_destination_after_incomplete_move() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        let source = source_dir.join("track.flac");
        let destination = destination_dir.join("track.flac");
        fs::write(&source, b"audio").expect("source");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");

        TEST_FORCE_COPY_THEN_DELETE_MOVE.with(|flag| flag.set(true));
        TEST_STOP_MOVE_AFTER_VERIFIED_PUBLICATION.with(|flag| flag.set(true));
        let first_failure = paste_filesystem_clipboard(
            &clipboard,
            &destination_dir,
            FileOperationPolicy::default(),
        )
        .expect_err("first public attempt must stop after verified publication");

        assert!(matches!(
            &first_failure.error,
            FilePickerError::DestinationCommittedMoveIncomplete { .. }
        ));
        assert_eq!(fs::read(&source).expect("retained source"), b"audio");
        assert_eq!(fs::read(&destination).expect("published destination"), b"audio");
        let retry_plan = first_failure
            .retry_plan
            .expect("public failure must return the exact retry mapping");
        assert_eq!(
            retry_plan.mappings,
            vec![PasteMapping {
                source: source.clone(),
                destination: destination.clone(),
            }]
        );

        let success = paste_filesystem_clipboard_with_retry(
            &clipboard,
            &destination_dir,
            FileOperationPolicy::default(),
            Some(&retry_plan),
        )
        .expect("public retry must verify and reuse the original destination");

        assert_eq!(success.mappings, retry_plan.mappings);
        assert!(!source.exists(), "retry must finish source cleanup");
        assert_eq!(fs::read(&destination).expect("single destination"), b"audio");
        assert!(!destination_dir.join("track 2.flac").exists());
        let entries = fs::read_dir(&destination_dir)
            .expect("destination entries")
            .collect::<Result<Vec<_>, _>>()
            .expect("destination entries");
        assert_eq!(entries.len(), 1, "public retry must not duplicate the destination");
    }

    #[test]
    fn public_retry_api_rejects_a_plan_for_another_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        let other_destination_dir = temp.path().join("other-destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        fs::create_dir(&other_destination_dir).expect("other destination dir");
        let source = source_dir.join("track.flac");
        fs::write(&source, b"audio").expect("source");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let stale_plan = PasteRetryPlan::from_plan(PastePlan {
            mode: FilePickerClipboardMode::Cut,
            mappings: vec![PasteMapping {
                source: source.clone(),
                destination: other_destination_dir.join("track.flac"),
            }],
        });

        let failure = paste_filesystem_clipboard_with_retry(
            &clipboard,
            &destination_dir,
            FileOperationPolicy::default(),
            Some(&stale_plan),
        )
        .expect_err("a retry plan for another destination must be rejected");

        assert!(matches!(
            &failure.error,
            FilePickerError::WrongSelectionMode(
                "retry plan does not match this cut clipboard and destination directory"
            )
        ));
        assert!(failure.retry_plan.is_none());
        assert_eq!(fs::read(&source).expect("source retained"), b"audio");
        assert!(
            fs::read_dir(&destination_dir)
                .expect("destination entries")
                .next()
                .is_none()
        );
    }

    #[test]
    fn retained_cut_retry_refuses_mismatched_original_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        let source = source_dir.join("track.flac");
        let destination = destination_dir.join("track.flac");
        fs::write(&source, b"source").expect("source");
        fs::write(&destination, b"different").expect("mismatched destination");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let retry_plan = PasteRetryPlan::from_plan(PastePlan {
            mode: FilePickerClipboardMode::Cut,
            mappings: vec![PasteMapping {
                source: source.clone(),
                destination: destination.clone(),
            }],
        });
        let (selected_plan, resume_existing) = plan_filesystem_paste_with_retry(
            &clipboard,
            &destination_dir,
            Some(&retry_plan),
        )
        .expect("retry plan");
        assert!(resume_existing);

        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
        let failure = execute_paste_plan_progress_with_resume(
            &selected_plan,
            FileOperationPolicy::default(),
            Some(&retry_plan),
            &mut progress,
        )
        .expect_err("mismatched retained destination must fail closed");
        assert!(matches!(
            failure.error,
            FilePickerError::DestinationCommittedMoveIncomplete { message, .. }
                if message.contains("originally planned retry destination differs")
        ));
        assert_eq!(fs::read(&source).expect("source retained"), b"source");
        assert_eq!(fs::read(&destination).expect("destination retained"), b"different");
        assert!(!destination_dir.join("track 2.flac").exists());
    }

    #[test]
    fn failed_committed_move_keeps_cut_clipboard_and_does_not_remap() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        let source = source_dir.join("track.flac");
        let destination = destination_dir.join("track.flac");
        fs::write(&source, b"audio").expect("source");
        fs::write(&destination, b"audio").expect("published destination");

        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: source_dir.clone(),
            ..FilePickerConfig::default()
        });
        picker.clipboard = Some(clipboard.clone());
        let failed_plan = PastePlan {
            mode: FilePickerClipboardMode::Cut,
            mappings: vec![PasteMapping {
                source: source.clone(),
                destination: destination.clone(),
            }],
        };
        picker.paste_task = Some(PickerPasteTask {
            progress: crate::FileTaskProgressState::new(
                crate::FileTaskKind::Move,
                "Moving files",
                picker.theme.clone(),
            ),
            receiver: None,
            control: Arc::new(AtomicU8::new(PASTE_CONTROL_RUNNING)),
            clipboard,
            target_dir: destination_dir.clone(),
            plan: failed_plan,
            retry_plan: None,
        });

        picker.finish_picker_paste(Err(PasteFailure {
            completed: Vec::new(),
            remaining_sources: vec![source.clone()],
            retry_plan: Some(PasteRetryPlan::from_plan(PastePlan {
                mode: FilePickerClipboardMode::Cut,
                mappings: vec![PasteMapping {
                    source: source.clone(),
                    destination: destination.clone(),
                }],
            })),
            warnings: Vec::new(),
            error: committed_operation_unverified(
                &source,
                &destination,
                "fixture verification failure".to_string(),
            ),
        }));

        assert_eq!(
            picker
                .clipboard
                .as_ref()
                .expect("retry clipboard retained")
                .paths(),
            &[source.clone()]
        );
        assert_eq!(picker.current_dir(), source_dir.as_path());
        let task = picker.paste_task.as_ref().expect("failed progress retained");
        assert!(task.progress.is_terminal());
        assert!(matches!(task.progress.phase, crate::FileTaskPhase::Failed));
        assert!(task.progress.status.contains("partially completed"));
    }

    #[test]
    fn completed_and_incomplete_roots_repair_current_and_history_independently() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_a = temp.path().join("source-a");
        let destination_a = temp.path().join("destination-a");
        let source_b = temp.path().join("source-b");
        let destination_b = temp.path().join("destination-b");
        fs::create_dir_all(destination_a.join("current")).expect("destination a");
        fs::create_dir_all(destination_b.join("history/future")).expect("destination b");

        let mapping_a = PasteMapping {
            source: source_a.clone(),
            destination: destination_a.clone(),
        };
        let mapping_b = PasteMapping {
            source: source_b.clone(),
            destination: destination_b.clone(),
        };
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source_a.clone(), source_b.clone()],
        )
        .expect("clipboard");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.current_dir = source_a.join("current");
        picker.history_back = vec![source_b.join("history")];
        picker.history_forward = vec![source_b.join("history/future")];
        picker.clipboard = Some(clipboard.clone());
        picker.paste_task = Some(PickerPasteTask {
            progress: crate::FileTaskProgressState::new(
                crate::FileTaskKind::Move,
                "Moving files",
                picker.theme.clone(),
            ),
            receiver: None,
            control: Arc::new(AtomicU8::new(PASTE_CONTROL_RUNNING)),
            clipboard,
            target_dir: temp.path().to_path_buf(),
            plan: PastePlan {
                mode: FilePickerClipboardMode::Cut,
                mappings: vec![mapping_a.clone(), mapping_b.clone()],
            },
            retry_plan: None,
        });

        picker.finish_picker_paste(Err(PasteFailure {
            completed: vec![mapping_a],
            remaining_sources: Vec::new(),
            retry_plan: None,
            warnings: Vec::new(),
            error: destination_committed_move_incomplete(
                &source_b,
                &destination_b,
                "fixture partial quarantine cleanup".to_string(),
            ),
        }));

        assert_eq!(picker.current_dir(), destination_a.join("current").as_path());
        assert_eq!(picker.history_back, vec![destination_b.join("history")]);
        assert_eq!(
            picker.history_forward,
            vec![destination_b.join("history/future")]
        );
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
    fn directory_first_local_move_keeps_atomic_rename_identity() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).expect("source");
        fs::write(source.join("track.flac"), b"audio").expect("track");
        let original = fs::symlink_metadata(&source).expect("source metadata");
        if crate::filesystem_identity_policy(&source)
            != crate::FilesystemIdentityPolicy::Strict
        {
            return;
        }

        let result = move_path_with_policy(
            &source,
            &destination,
            FileOperationPolicy::default(),
        );
        match result {
            Ok(()) | Err(FilePickerError::OperationCommittedWithWarning { .. }) => {}
            Err(error) => panic!("local directory move failed: {error}"),
        }

        let moved = fs::symlink_metadata(&destination).expect("destination metadata");
        assert_eq!(
            (original.dev(), original.ino()),
            (moved.dev(), moved.ino()),
            "known local directory moves must retain the atomic rename fast path"
        );
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

    #[test]
    fn rename_no_replace_degrades_when_atomic_primitive_is_unsupported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("destination.flac");
        fs::write(&source, b"audio").expect("source");

        let mode = rename_no_replace_with(&source, &destination, |_source, _destination| {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "fixture: no atomic no-clobber rename",
            ))
        })
        .expect("checked fallback should commit");

        assert_eq!(mode, RenameNoReplaceMode::CheckedBestEffort);
        assert!(!source.exists());
        assert_eq!(fs::read(&destination).expect("destination"), b"audio");
    }

    #[test]
    fn rename_no_replace_degraded_path_still_refuses_existing_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("destination.flac");
        fs::write(&source, b"source").expect("source");
        fs::write(&destination, b"existing").expect("destination");

        let error = rename_no_replace_with(&source, &destination, |_source, _destination| {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "fixture: no atomic no-clobber rename",
            ))
        })
        .expect_err("existing destination must be preserved");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&source).expect("source preserved"), b"source");
        assert_eq!(fs::read(&destination).expect("destination preserved"), b"existing");
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

    fn assert_completed_operation(result: Result<(), FilePickerError>) {
        match result {
            Ok(()) | Err(FilePickerError::OperationCommittedWithWarning { .. }) => {}
            other => panic!("operation did not complete: {other:?}"),
        }
    }

    fn strong_policy() -> FileOperationPolicy {
        FileOperationPolicy {
            verification: VerificationMode::Strong,
            ..FileOperationPolicy::default()
        }
    }

    #[test]
    fn same_filesystem_native_rename_has_one_stat_only_walk_and_zero_content_reads() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).expect("source dir");
        fs::write(source.join("track.flac"), b"audio").expect("track");
        let mut io = crate::FileOperationIoCounters::default();
        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());

        assert_completed_operation(move_path_with_policy_progress_accounted(
            &source,
            &destination,
            FileOperationPolicy::default(),
            &mut progress,
            &mut io,
        ));

        assert_eq!(io.rename_attempts, 1);
        assert_eq!(io.bytes_copied, 0);
        assert_eq!(io.source_bytes_hashed, 0);
        assert_eq!(io.destination_bytes_hashed, 0);
        assert_eq!(
            io.source_tree_walks,
            1,
            "standard native move performs one stat-only manifest walk",
        );
        assert_eq!(io.destination_tree_walks, 0);
        assert_eq!(io.destination_entry_verification_passes, 0);
        assert!(!source.exists());
        assert_eq!(fs::read(destination.join("track.flac")).expect("moved"), b"audio");
    }

    #[test]
    fn reduced_semantics_select_rename_before_copy_fallback() {
        let reduced = crate::FilesystemCapabilities {
            semantics: crate::FilesystemSemantics::NetworkOrReduced,
            stable_path_identity: crate::CapabilitySupport::Unsupported,
            nanosecond_timestamps: crate::CapabilitySupport::Unsupported,
            extended_attributes: crate::CapabilitySupport::Unknown,
            directory_sync: crate::CapabilitySupport::Unknown,
            atomic_no_replace_rename: crate::CapabilitySupport::Unsupported,
            filesystem_type: Some(0x6573_5546),
        };
        assert_eq!(
            initial_move_route(reduced, reduced, false),
            InitialMoveRoute::NativeRename,
            "reduced inode/timestamp semantics must change the proof, not bypass native rename"
        );
    }

    #[test]
    fn standard_copy_performs_one_payload_read_without_content_verification_passes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        let destination = temp.path().join("copy.flac");
        let bytes = b"deterministic audio payload";
        fs::write(&source, bytes).expect("source");
        let mut io = crate::FileOperationIoCounters::default();
        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());

        let outcome = safe_copy_path_progress_with_notices_accounted(
            &source,
            &destination,
            FileOperationPolicy::default(),
            &mut progress,
            &mut io,
        )
        .expect("standard copy");

        assert_eq!(io.bytes_copied, bytes.len() as u64);
        assert_eq!(io.source_bytes_hashed, 0);
        assert_eq!(io.destination_bytes_hashed, 0);
        assert_eq!(io.bytes_redundantly_rehashed, 0);
        assert_eq!(io.source_tree_walks, 1);
        assert_eq!(io.destination_tree_walks, 0);
        assert_eq!(io.destination_entry_verification_passes, 0);
        assert_eq!(io.file_sync_calls, 1, "the writable staged root file is synchronized before publication");
        assert_eq!(io.directory_sync_calls, 1, "only the destination parent is synchronized");
        assert_eq!(outcome.source_manifest.verification(), VerificationMode::Standard);
        assert!(!outcome.source_manifest.has_content_digests());
        assert_eq!(outcome.destination_manifest.verification(), VerificationMode::Standard);
        assert_eq!(fs::read(&destination).expect("destination"), bytes);
    }

    #[test]
    #[ignore = "acceptance-scale 128 MiB release gate; run explicitly to avoid burdening the ordinary suite"]
    fn acceptance_scale_standard_native_move_and_copy_pin_16_files_128_mib() {
        const FILE_COUNT: usize = 16;
        const FILE_BYTES: u64 = 8 * 1024 * 1024;
        const TOTAL_BYTES: u64 = FILE_COUNT as u64 * FILE_BYTES;

        let temp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            crate::filesystem_identity_policy(temp.path()),
            crate::FilesystemIdentityPolicy::Strict,
            "acceptance-scale fixture must run on a strict local mount",
        );
        let source = temp.path().join("source-album");
        let moved = temp.path().join("moved-album");
        let copied = temp.path().join("copied-album");
        fs::create_dir(&source).expect("source album");
        for index in 0..FILE_COUNT {
            let path = source.join(format!("track-{index:02}.bin"));
            let payload = vec![index as u8; FILE_BYTES as usize];
            fs::write(&path, payload).expect("write acceptance-scale track bytes");
        }

        let mut move_io = crate::FileOperationIoCounters::default();
        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
        assert_completed_operation(move_path_with_policy_progress_accounted(
            &source,
            &moved,
            FileOperationPolicy::default(),
            &mut progress,
            &mut move_io,
        ));
        assert_eq!(move_io.rename_attempts, 1);
        assert_eq!(move_io.source_tree_walks, 1);
        assert_eq!(move_io.bytes_copied, 0);
        assert_eq!(move_io.source_bytes_hashed, 0);
        assert_eq!(move_io.destination_bytes_hashed, 0);

        let mut copy_io = crate::FileOperationIoCounters::default();
        let outcome = safe_copy_path_progress_with_notices_accounted(
            &moved,
            &copied,
            FileOperationPolicy::default(),
            &mut progress,
            &mut copy_io,
        )
        .expect("acceptance-scale standard copy");
        assert_eq!(copy_io.bytes_copied, TOTAL_BYTES);
        assert_eq!(copy_io.source_bytes_hashed, 0);
        assert_eq!(copy_io.destination_bytes_hashed, 0);
        assert_eq!(copy_io.bytes_redundantly_rehashed, 0);
        assert_eq!(copy_io.source_tree_walks, 1);
        assert_eq!(copy_io.destination_tree_walks, 0);
        assert_eq!(copy_io.destination_entry_verification_passes, 0);
        assert_eq!(
            copy_io.file_sync_calls,
            0,
            "directory publication must not sync each of {FILE_COUNT} files: {copy_io:?}",
        );
        assert!(
            copy_io.directory_sync_calls <= 2,
            "standard directory copy syncs only the published root and its parent: {copy_io:?}",
        );
        assert_eq!(outcome.source_manifest.verification(), VerificationMode::Standard);
        assert!(!outcome.source_manifest.has_content_digests());
        assert_eq!(outcome.destination_manifest.verification(), VerificationMode::Standard);
        for index in 0..FILE_COUNT {
            assert_eq!(
                fs::metadata(copied.join(format!("track-{index:02}.bin")))
                    .expect("stat copied acceptance track")
                    .len(),
                FILE_BYTES,
            );
        }
    }

    #[test]
    fn standard_copy_then_delete_remains_zero_content_read_on_portable_mounts() {
        let _portable = crate::source_guard::test_override_filesystem_capabilities(
            crate::FilesystemCapabilities {
                semantics: crate::FilesystemSemantics::NetworkOrReduced,
                stable_path_identity: crate::CapabilitySupport::Unsupported,
                nanosecond_timestamps: crate::CapabilitySupport::Unsupported,
                extended_attributes: crate::CapabilitySupport::Unknown,
                directory_sync: crate::CapabilitySupport::Supported,
                atomic_no_replace_rename: crate::CapabilitySupport::Supported,
                filesystem_type: Some(0x6573_5546),
            },
        );
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        let destination = temp.path().join("moved.flac");
        let bytes = b"deterministic audio payload";
        fs::write(&source, bytes).expect("source");
        TEST_FORCE_COPY_THEN_DELETE_MOVE.with(|flag| flag.set(true));
        let mut io = crate::FileOperationIoCounters::default();
        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());

        assert_completed_operation(move_path_with_policy_progress_accounted(
            &source,
            &destination,
            FileOperationPolicy::default(),
            &mut progress,
            &mut io,
        ));

        assert_eq!(io.bytes_copied, bytes.len() as u64);
        assert_eq!(io.source_bytes_hashed, 0);
        assert_eq!(io.destination_bytes_hashed, 0);
        assert_eq!(io.bytes_redundantly_rehashed, 0);
        assert_eq!(io.destination_tree_walks, 0);
        assert_eq!(io.file_sync_calls, 1, "the writable staged root file is synchronized before publication");
        assert_eq!(
            io.directory_sync_calls, 2,
            "standard mode synchronizes the destination parent and the source parent once each"
        );
        assert!(!source.exists());
        assert_eq!(fs::read(&destination).expect("destination"), bytes);
    }

    #[test]
    fn unavoidable_file_move_fuses_copy_hash_and_uses_one_destination_tree_pass() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        let destination = temp.path().join("moved.flac");
        let bytes = b"deterministic audio payload";
        fs::write(&source, bytes).expect("source");
        let portable_destination = crate::filesystem_identity_policy(temp.path())
            == crate::FilesystemIdentityPolicy::ContentVerifiedPortable;
        TEST_FORCE_COPY_THEN_DELETE_MOVE.with(|flag| flag.set(true));
        let mut io = crate::FileOperationIoCounters::default();
        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());

        assert_completed_operation(move_path_with_policy_progress_accounted(
            &source,
            &destination,
            strong_policy(),
            &mut progress,
            &mut io,
        ));

        let len = bytes.len() as u64;
        assert_eq!(io.bytes_copied, len);
        let expected_destination_hash = len.saturating_add(
            portable_destination.then_some(len).unwrap_or(0),
        );
        assert_eq!(
            io.destination_bytes_hashed,
            expected_destination_hash,
            "publication verification reads the destination once; reduced-semantics cleanup performs one irreducible final rehash"
        );
        assert_eq!(io.bytes_redundantly_rehashed, 0);
        assert_eq!(io.destination_tree_walks, 1);
        assert_eq!(
            io.destination_entry_verification_passes,
            1,
            "cleanup performs one separately counted stability pass; this file route performs no directory-membership enumeration"
        );
        assert_eq!(io.source_tree_walks, 2, "copy traversal plus fused verify/delete traversal");
        assert_eq!(
            io.source_bytes_hashed,
            len.saturating_mul(2),
            "copy-time hashing and the final post-quarantine source digest are the two authoritative source reads"
        );
        assert_eq!(io.file_sync_calls, 1, "the staged file is synchronized once");
        assert_eq!(
            io.directory_sync_calls, 3,
            "publication, quarantine, and final source removal are distinct durable boundaries"
        );
        assert_eq!(fs::read(&destination).expect("destination"), bytes);
        assert!(!source.exists());
    }

    #[test]
    fn destination_mutation_after_publication_revokes_source_cleanup_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"original").expect("source");
        TEST_FORCE_COPY_THEN_DELETE_MOVE.with(|flag| flag.set(true));
        let mut io = crate::FileOperationIoCounters::default();
        let mut mutation_injected = false;
        // The completed=true event for an entry fires only after its source
        // was already deleted, so the mutation must land in the post-quarantine
        // window: source is gone (quarantined as "payload"), destination is
        // published, and the pre-deletion stability gate has yet to run.
        let mut progress =
            |_callback_source: &Path, reported_destination: &Path, _bytes: u64, completed: bool| {
                // The post-quarantine window is identified by the source having
                // been renamed away while the destination is committed; the
                // callback's source-path shape varies by verification flow, so
                // it must not be part of the predicate.
                if !completed
                    && !mutation_injected
                    && reported_destination == destination.as_path()
                    && destination.exists()
                    && !source.exists()
                {
                    fs::write(&destination, b"replaced")
                        .expect("same-length post-verification mutation");
                    // A same-size rewrite inside one kernel timestamp tick is
                    // metadata-invisible to the strict no-content-read gate
                    // (documented residual, same class as the rename TOCTOU
                    // disclosure). Move the version token deterministically the
                    // way any real-world mutation eventually does.
                    let mutated = fs::File::options()
                        .write(true)
                        .open(&destination)
                        .expect("reopen mutated destination");
                    mutated
                        .set_modified(
                            std::time::SystemTime::now() + std::time::Duration::from_secs(2),
                        )
                        .expect("advance mutated destination mtime");
                    mutation_injected = true;
                }
                Ok(())
            };

        let error = move_path_with_policy_progress_accounted(
            &source,
            &destination,
            strong_policy(),
            &mut progress,
            &mut io,
        )
        .expect_err("stale destination proof must prevent source deletion");

        assert!(mutation_injected, "mutation must occur after publication");
        assert!(matches!(
            error,
            FilePickerError::DestinationCommittedMoveIncomplete { .. }
        ));
        assert_eq!(fs::read(&source).expect("source restored"), b"original");
        assert_eq!(fs::read(&destination).expect("mutated destination retained"), b"replaced");
        assert_eq!(io.destination_tree_walks, 1);
        assert_eq!(io.destination_entry_verification_passes, 1);
    }

    #[test]
    fn retry_plan_with_unattempted_roots_keeps_exact_paths_and_native_rename() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        let first = source_dir.join("first.flac");
        let second = source_dir.join("second.flac");
        fs::write(&first, b"first").expect("first");
        fs::write(&second, b"second").expect("second");
        let plan = PastePlan {
            mode: FilePickerClipboardMode::Cut,
            mappings: vec![
                PasteMapping {
                    source: first.clone(),
                    destination: destination_dir.join("first.flac"),
                },
                PasteMapping {
                    source: second.clone(),
                    destination: destination_dir.join("second.flac"),
                },
            ],
        };
        let retry = PasteRetryPlan::from_plan(plan.clone());
        let mut io = crate::FileOperationIoCounters::default();
        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());

        let success = execute_paste_plan_progress_with_resume_accounted(
            &plan,
            FileOperationPolicy::default(),
            Some(&retry),
            &mut progress,
            &mut io,
        )
        .expect("unattempted exact-plan roots must use the ordinary rename-first route");

        assert_eq!(success.mappings, plan.mappings);
        assert_eq!(io.rename_attempts, 2);
        assert_eq!(io.bytes_copied, 0);
        assert_eq!(io.source_bytes_hashed, 0);
        assert_eq!(io.destination_bytes_hashed, 0);
        assert!(!destination_dir.join("first 2.flac").exists());
        assert!(!destination_dir.join("second 2.flac").exists());
        assert!(!first.exists());
        assert!(!second.exists());
    }

    #[test]
    fn retry_of_verified_destination_performs_no_recopy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&destination_dir).expect("destination dir");
        fs::write(&source, b"audio").expect("source");
        let clipboard = FilesystemClipboard::new(
            FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let plan = plan_filesystem_paste(&clipboard, &destination_dir).expect("plan");
        TEST_FORCE_COPY_THEN_DELETE_MOVE.with(|flag| flag.set(true));
        TEST_STOP_MOVE_AFTER_VERIFIED_PUBLICATION.with(|flag| flag.set(true));
        let mut first_io = crate::FileOperationIoCounters::default();
        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
        let failure = execute_paste_plan_progress_with_resume_accounted(
            &plan,
            strong_policy(),
            None,
            &mut progress,
            &mut first_io,
        )
        .expect_err("first move must stop after verified publication");
        let retry = failure.retry_plan.expect("exact retry plan");
        let mut retry_io = crate::FileOperationIoCounters::default();
        let mut retry_progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());
        let recovered = execute_paste_plan_progress_with_resume_accounted(
            retry.plan(),
            strong_policy(),
            Some(&retry),
            &mut retry_progress,
            &mut retry_io,
        )
        .expect("retry");

        assert_eq!(recovered.mappings, retry.mappings);
        assert!(
            retry.recovery_for(&source).is_some(),
            "failure must retain authoritative source/destination proof"
        );
        assert_eq!(retry_io.bytes_copied, 0, "verified retry must not recopy data");
        let expected_destination_rehash = if crate::filesystem_identity_policy(
            &destination_dir.join("track.flac"),
        ) == crate::FilesystemIdentityPolicy::ContentVerifiedPortable
        {
            (b"audio".len() as u64).saturating_mul(2)
        } else {
            0
        };
        assert_eq!(
            retry_io.destination_bytes_hashed,
            expected_destination_rehash,
            "strict mounts reuse exact version evidence; portable mounts require one retry proof read plus one final pre-delete rehash"
        );
        assert_eq!(retry_io.destination_tree_walks, 1);
        assert_eq!(retry_io.destination_entry_verification_passes, 1);
        assert!(!destination_dir.join("track 2.flac").exists());
        assert_eq!(fs::read(destination_dir.join("track.flac")).expect("destination"), b"audio");
        assert!(!source.exists());
    }

    #[test]
    fn unavoidable_directory_move_has_bounded_tree_walks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album");
        let destination = temp.path().join("moved-album");
        fs::create_dir(&source).expect("source dir");
        fs::create_dir(source.join("disc")).expect("disc");
        fs::write(source.join("disc").join("track.flac"), b"audio").expect("track");
        let portable_destination = crate::filesystem_identity_policy(temp.path())
            == crate::FilesystemIdentityPolicy::ContentVerifiedPortable;
        TEST_FORCE_COPY_THEN_DELETE_MOVE.with(|flag| flag.set(true));
        let mut io = crate::FileOperationIoCounters::default();
        let mut progress =
            |_source: &Path, _destination: &Path, _bytes: u64, _completed: bool| Ok(());

        assert_completed_operation(move_path_with_policy_progress_accounted(
            &source,
            &destination,
            strong_policy(),
            &mut progress,
            &mut io,
        ));

        assert_eq!(io.source_tree_walks, 2);
        assert_eq!(io.destination_tree_walks, 1);
        assert_eq!(io.destination_entry_verification_passes, 1);
        assert_eq!(io.bytes_redundantly_rehashed, 0);
        let expected_source_hash = 5u64.saturating_add(
            portable_destination.then_some(5).unwrap_or(0),
        );
        assert_eq!(
            io.source_bytes_hashed,
            expected_source_hash,
            "strict descendant ctime tokens avoid a second content read; portable mounts require the final digest"
        );
        let expected_destination_hash = 5u64.saturating_add(
            portable_destination.then_some(5).unwrap_or(0),
        );
        assert_eq!(io.destination_bytes_hashed, expected_destination_hash);
        assert_eq!(io.file_sync_calls, 1);
        assert_eq!(
            io.directory_sync_calls, 5,
            "two staged directories plus publication, quarantine, and final removal"
        );
        assert_eq!(fs::read(destination.join("disc/track.flac")).expect("track"), b"audio");
        assert!(!source.exists());
    }

    #[test]
    fn marked_file_confirmation_filters_directories_and_emits_visible_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = temp.path().join("00-folder");
        let first = temp.path().join("01-first.flac");
        let second = temp.path().join("02-second.flac");
        fs::create_dir(&directory).expect("directory");
        fs::write(&first, b"first").expect("first");
        fs::write(&second, b"second").expect("second");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::FilesOrDirectories,
            ..FilePickerConfig::default()
        });

        // Deliberately use gesture order opposite to visible sort order and
        // include a directory mark. Completion must be deterministic.
        picker.replace_multi_selected(vec![second.clone(), directory, first.clone()]);
        assert_eq!(picker.selection_confirmation_label().as_deref(), Some("Select 2 Files"));
        assert_eq!(
            picker.accept_current_selection(),
            FilePickerAction::SelectedMany(vec![first, second])
        );
        assert_eq!(picker.last_selection_ignored_directories(), 1);
    }

    #[test]
    fn refresh_discloses_each_marked_path_that_disappeared_exactly_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("01-first.flac");
        let second = temp.path().join("02-second.flac");
        fs::write(&first, b"first").expect("first");
        fs::write(&second, b"second").expect("second");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.replace_multi_selected(vec![first.clone(), second.clone()]);

        fs::remove_file(&second).expect("remove marked file");
        picker.refresh();

        assert_eq!(picker.multi_selected, vec![first]);
        assert_eq!(picker.take_last_selection_dropped_invisible(), 1);
        assert_eq!(picker.take_last_selection_dropped_invisible(), 0);
    }

    #[test]
    fn large_range_selection_uses_one_membership_probe_per_visible_entry() {
        use std::cell::Cell;

        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Files,
            ..FilePickerConfig::default()
        });
        const COUNT: usize = 20_000;
        picker.entries = (0..COUNT)
            .map(|index| FilePickerEntry {
                name: format!("{index:05}.flac"),
                path: temp.path().join(format!("{index:05}.flac")),
                is_dir: false,
                size: Some(1),
                file_type: "FLAC".to_string(),
                modified: None,
            })
            .collect();
        picker.visible_path_indices = picker
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.path.clone(), index))
            .collect();
        picker.file_cursor = COUNT - 1;
        picker.range_anchor = Some(picker.entries[0].path.clone());

        assert!(picker.mark_range_to_index(COUNT - 1));
        assert_eq!(picker.multi_selected_lookup.len(), COUNT);
        assert_eq!(picker.multi_selected.len(), COUNT);
        assert_eq!(picker.effective_selected_count(), COUNT);
        assert_eq!(
            picker.selection_confirmation_label().as_deref(),
            Some("Select 20000 Files")
        );

        let probes = Cell::new(0usize);
        let (marked_files, marked_directories) = picker.marked_visible_counts_with(|path| {
            probes.set(probes.get().saturating_add(1));
            picker.multi_selected_lookup.contains(path)
        });
        assert_eq!(probes.get(), COUNT, "visible mark counting must be one linear scan");
        assert_eq!((marked_files, marked_directories), (COUNT, 0));

        let (ordered, ignored_directories) = picker.marked_files_in_visible_order();
        assert_eq!(ordered.len(), COUNT);
        assert_eq!(ignored_directories, 0);
        assert_eq!(ordered.first(), picker.entries.first().map(|entry| &entry.path));
        assert_eq!(ordered.last(), picker.entries.last().map(|entry| &entry.path));
    }

    #[test]
    fn directory_only_marks_fall_back_to_cursor_selection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = temp.path().join("folder");
        let file = temp.path().join("track.flac");
        fs::create_dir(&directory).expect("directory");
        fs::write(&file, b"audio").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::FilesOrDirectories,
            ..FilePickerConfig::default()
        });
        let file_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == file)
            .expect("file visible");
        picker.set_file_cursor(file_index, 8);
        picker.replace_multi_selected(vec![directory]);

        assert_eq!(picker.selection_confirmation_label().as_deref(), Some("Select File"));
        assert_eq!(picker.accept_current_selection(), FilePickerAction::Selected(file));
        assert_eq!(picker.last_selection_ignored_directories(), 1);
    }

    #[test]
    fn contextual_confirmation_labels_cover_all_selection_states() {
        let temp = tempfile::tempdir().expect("tempdir");
        let directory = temp.path().join("folder");
        let file = temp.path().join("track.flac");
        fs::create_dir(&directory).expect("directory");
        fs::write(&file, b"audio").expect("file");

        let mut mixed = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::FilesOrDirectories,
            ..FilePickerConfig::default()
        });
        let directory_index = mixed
            .entries()
            .iter()
            .position(|entry| entry.path == directory)
            .expect("directory visible");
        let file_index = mixed
            .entries()
            .iter()
            .position(|entry| entry.path == file)
            .expect("file visible");
        mixed.set_file_cursor(directory_index, 8);
        assert_eq!(mixed.selection_confirmation_label().as_deref(), Some("Select Folder"));
        mixed.set_file_cursor(file_index, 8);
        assert_eq!(mixed.selection_confirmation_label().as_deref(), Some("Select File"));
        mixed.replace_multi_selected(vec![file.clone()]);
        assert_eq!(mixed.selection_confirmation_label().as_deref(), Some("Select 1 File"));

        let mut files_only = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Files,
            ..FilePickerConfig::default()
        });
        let directory_index = files_only
            .entries()
            .iter()
            .position(|entry| entry.path == directory)
            .expect("directory visible");
        files_only.set_file_cursor(directory_index, 8);
        assert_eq!(files_only.selection_confirmation_label(), None);

        let directories = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Directories,
            ..FilePickerConfig::default()
        });
        assert_eq!(
            directories.selection_confirmation_label().as_deref(),
            Some("Select Folder")
        );
    }

}

#[cfg(test)]
mod case_rename_transaction_tests {
    use super::*;

    #[test]
    fn picker_case_rename_transaction_supports_case_only_names() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("foo.flac");
        let destination = temp.path().join("Foo.flac");
        fs::write(&source, b"audio").expect("fixture");

        let result = execute_picker_case_rename_transaction(
            std::slice::from_ref(&source),
            |name| {
                if name == "foo.flac" {
                    "Foo.flac".to_string()
                } else {
                    name.to_string()
                }
            },
        )
        .expect("case-only rename");

        assert_eq!(result, vec![destination.clone()]);
        assert_eq!(fs::read(&destination).expect("renamed file"), b"audio");
    }

    #[test]
    fn picker_case_rename_transaction_stages_swaps() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("A.flac");
        let b = temp.path().join("B.flac");
        fs::write(&a, b"A bytes").expect("A");
        fs::write(&b, b"B bytes").expect("B");

        let result = execute_picker_case_rename_transaction(&[a.clone(), b.clone()], |name| {
            match name {
                "A.flac" => "B.flac".to_string(),
                "B.flac" => "A.flac".to_string(),
                other => other.to_string(),
            }
        })
        .expect("swap");

        assert_eq!(result, vec![b.clone(), a.clone()]);
        assert_eq!(fs::read(&a).expect("A after swap"), b"B bytes");
        assert_eq!(fs::read(&b).expect("B after swap"), b"A bytes");
    }

    #[test]
    fn picker_case_rename_collision_is_rejected_before_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("A.flac");
        let b = temp.path().join("B.flac");
        fs::write(&a, b"A bytes").expect("A");
        fs::write(&b, b"B bytes").expect("B");

        let error = execute_picker_case_rename_transaction(&[a.clone(), b.clone()], |_| {
            "same.flac".to_string()
        })
        .expect_err("duplicate destination");

        assert!(matches!(error, FilePickerError::DestinationExists(_)));
        assert_eq!(fs::read(&a).expect("A unchanged"), b"A bytes");
        assert_eq!(fs::read(&b).expect("B unchanged"), b"B bytes");
    }
}

#[cfg(test)]
mod exact_replay_authority_tests {
    use super::*;

    fn strong_policy() -> FileOperationPolicy {
        FileOperationPolicy {
            verification: VerificationMode::Strong,
            ..FileOperationPolicy::default()
        }
    }

    fn portable_replay_capabilities() -> crate::FilesystemCapabilities {
        crate::FilesystemCapabilities {
            semantics: crate::FilesystemSemantics::NetworkOrReduced,
            stable_path_identity: crate::CapabilitySupport::Supported,
            nanosecond_timestamps: crate::CapabilitySupport::Unsupported,
            extended_attributes: crate::CapabilitySupport::Unknown,
            directory_sync: crate::CapabilitySupport::Supported,
            atomic_no_replace_rename: crate::CapabilitySupport::Supported,
            filesystem_type: Some(0x6573_5546),
        }
    }

    fn retained_same_path_proof(
        path: &Path,
        verification: VerificationMode,
    ) -> crate::FileTaskRootProof {
        let source_manifest = crate::capture_manifest_with_mode(path, verification)
            .expect("capture retained source");
        let destination_manifest = source_manifest.destination_identity_for_same_tree();
        crate::FileTaskRootProof {
            source_manifest,
            destination_manifest,
        }
    }

    #[test]
    fn copy_replay_rejects_changed_source_before_publication() {
        let _portable = crate::source_guard::test_override_filesystem_capabilities(
            portable_replay_capabilities(),
        );
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("destination.flac");
        fs::write(&source, b"authorized bytes").expect("source");
        let expected = retained_same_path_proof(&source, VerificationMode::Strong);
        fs::write(&source, b"altered content!").expect("replace source");

        let plan = PastePlan {
            mode: FilePickerClipboardMode::Copy,
            mappings: vec![PasteMapping {
                source: source.clone(),
                destination: destination.clone(),
            }],
        };
        let failure = execute_exact_paste_plan_with_proofs_and_expected_sources(
            &plan,
            strong_policy(),
            &[expected],
        )
        .expect_err("changed replay source must be refused");

        let failure = failure.to_string();
        assert!(
            failure.contains("replay source content changed"),
            "matching strong authority must reach the digest comparison: {failure}",
        );
        assert!(
            !failure.contains("verification authority mismatch"),
            "test must reach the matching-strong replay checks: {failure}",
        );
        assert!(!destination.exists(), "unauthorized bytes must never be published");
        assert_eq!(fs::read(&source).expect("source retained"), b"altered content!");
    }

    #[test]
    fn move_replay_rejects_changed_source_before_namespace_mutation() {
        let _portable = crate::source_guard::test_override_filesystem_capabilities(
            portable_replay_capabilities(),
        );
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("destination.flac");
        fs::write(&source, b"authorized bytes").expect("source");
        let expected = retained_same_path_proof(&source, VerificationMode::Strong);
        fs::write(&source, b"altered content!").expect("replace source");

        let plan = PastePlan {
            mode: FilePickerClipboardMode::Cut,
            mappings: vec![PasteMapping {
                source: source.clone(),
                destination: destination.clone(),
            }],
        };
        let failure = execute_exact_paste_plan_with_proofs_and_expected_sources(
            &plan,
            strong_policy(),
            &[expected],
        )
        .expect_err("changed replay source must be refused");

        let failure = failure.to_string();
        assert!(
            failure.contains("replay source content changed"),
            "matching strong authority must reach the digest comparison: {failure}",
        );
        assert!(
            !failure.contains("verification authority mismatch"),
            "test must reach the matching-strong replay checks: {failure}",
        );
        assert!(!destination.exists(), "unauthorized object must not be moved");
        assert_eq!(fs::read(&source).expect("source retained"), b"altered content!");
    }
}
