#![deny(unsafe_code)]

use same_file::Handle as SameFileHandle;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[cfg(test)]
thread_local! {
    static TEST_CONFIG_PATH_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        std::cell::RefCell::new(None);
    static TEST_CONFIG_PUBLICATION_SYNC_FAILURE: std::cell::RefCell<Option<String>> =
        std::cell::RefCell::new(None);
    static TEST_LOCK_MARKER_SYNC_FAILURE: std::cell::RefCell<Option<String>> =
        std::cell::RefCell::new(None);
    static TEST_STALE_ARTIFACT_REMOVE_FAILURE: std::cell::RefCell<Option<(PathBuf, String)>> =
        std::cell::RefCell::new(None);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TonepoetConfig {
    pub conversion: ConversionSettings,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
    #[serde(default)]
    pub browsing: BrowsingConfig,
    #[serde(default)]
    pub file_operations: FileOperationsConfig,
    #[serde(default)]
    pub naming: NamingConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamingConfig {
    /// Strip trailing dots and spaces from final path components for Windows
    /// interoperability. Disabled by default so canonical metadata is lossless.
    #[serde(default)]
    pub windows_portable: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileOperationStatusVerbosity {
    Quiet,
    Verbose,
}

impl Default for FileOperationStatusVerbosity {
    fn default() -> Self {
        Self::Quiet
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileOperationsConfig {
    #[serde(default)]
    pub verification: tui_file_picker::VerificationMode,
    #[serde(default)]
    pub status_verbosity: FileOperationStatusVerbosity,
    #[serde(default)]
    pub auto_close_progress: bool,
    /// Seconds without a progress event before the copy/move overlay reports a stall.
    #[serde(default = "default_file_operation_stall_timeout_secs")]
    pub stall_timeout_secs: u64,
}

impl Default for FileOperationsConfig {
    fn default() -> Self {
        Self {
            verification: tui_file_picker::VerificationMode::Standard,
            status_verbosity: FileOperationStatusVerbosity::Quiet,
            auto_close_progress: false,
            stall_timeout_secs: default_file_operation_stall_timeout_secs(),
        }
    }
}

const fn default_file_operation_stall_timeout_secs() -> u64 {
    crate::tui::file_task_runtime::DEFAULT_FILE_TASK_STALL_TIMEOUT_SECS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    #[serde(default)]
    pub browsing: BrowsingPerformanceConfig,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            browsing: BrowsingPerformanceConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowsingPerformanceConfig {
    /// Controls automatic archive content listing in Browse.
    /// Valid values: "auto", "always", "never". Unknown values behave as "auto".
    #[serde(default = "default_archive_listing_mode")]
    pub archive_listing: String,
    /// Archive listing timeout in seconds. 0 disables the timeout.
    #[serde(default = "default_archive_listing_timeout")]
    pub archive_listing_timeout: u64,
}

impl Default for BrowsingPerformanceConfig {
    fn default() -> Self {
        Self {
            archive_listing: default_archive_listing_mode(),
            archive_listing_timeout: default_archive_listing_timeout(),
        }
    }
}

fn default_archive_listing_mode() -> String {
    "auto".to_string()
}

fn default_archive_listing_timeout() -> u64 {
    30
}


/// Browse-screen view and interaction preferences.
///
/// This table is intentionally separate from `[performance.browsing]`, which
/// owns operational concerns such as archive-listing policy and timeouts.
/// Deserialization accepts missing fields for backwards compatibility; callers
/// should use [`BrowsingConfig::normalized`] before applying user-edited values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowsingConfig {
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default = "default_browse_columns")]
    pub columns: Vec<String>,
    #[serde(default = "default_browse_sort")]
    pub default_sort: String,
    #[serde(default = "default_browse_sort_dir")]
    pub default_sort_dir: String,
    #[serde(default = "default_browse_filter")]
    pub default_filter: String,
    /// Whether the Explore pane is present in Browse layout at all.
    /// This is independent from `layout_explore`, which preserves the
    /// enabled pane's collapsed/open state.
    #[serde(default = "default_true")]
    pub layout_explore_enabled: bool,
    /// Whether the Info pane is present in Browse layout at all.
    /// This is independent from `layout_info`, which preserves the
    /// enabled pane's collapsed/open state.
    #[serde(default = "default_true")]
    pub layout_info_enabled: bool,
    #[serde(default = "default_browse_layout_open")]
    pub layout_explore: String,
    #[serde(default = "default_browse_layout_open")]
    pub layout_info: String,
    /// Maximum number of recursive search results retained after global sorting.
    /// The worker scores every match first; this cap is applied only after sort
    /// so late high-quality matches are not discarded by walk order.
    #[serde(default = "default_browse_search_result_cap")]
    pub search_result_cap: usize,
}

impl Default for BrowsingConfig {
    fn default() -> Self {
        Self {
            show_hidden: false,
            columns: default_browse_columns(),
            default_sort: default_browse_sort(),
            default_sort_dir: default_browse_sort_dir(),
            default_filter: default_browse_filter(),
            layout_explore_enabled: true,
            layout_info_enabled: true,
            layout_explore: default_browse_layout_open(),
            layout_info: default_browse_layout_open(),
            search_result_cap: default_browse_search_result_cap(),
        }
    }
}

impl BrowsingConfig {
    pub fn normalized(&self) -> Self {
        let mut columns = Vec::new();
        for raw in &self.columns {
            let column = normalize_browse_token(raw);
            if is_supported_browse_column(&column) && !columns.iter().any(|c| c == &column) {
                columns.push(column);
            }
        }
        if columns.is_empty() {
            columns = default_browse_columns();
        } else if !columns.iter().any(|c| c == "name") {
            columns.insert(0, "name".to_string());
        }

        let default_sort = normalize_browse_token(&self.default_sort);
        let default_sort = if is_supported_browse_sort(&default_sort) {
            default_sort
        } else {
            default_browse_sort()
        };

        let default_sort_dir = normalize_browse_token(&self.default_sort_dir);
        let default_sort_dir = match default_sort_dir.as_str() {
            "asc" | "ascending" => "asc".to_string(),
            "desc" | "descending" => "desc".to_string(),
            _ => default_browse_sort_dir(),
        };

        let default_filter = normalize_browse_token(&self.default_filter);
        let default_filter = if is_supported_browse_filter(&default_filter) {
            default_filter
        } else {
            default_browse_filter()
        };

        let layout_explore = normalize_layout_state(&self.layout_explore);
        let layout_info = normalize_layout_state(&self.layout_info);
        let search_result_cap = normalize_search_result_cap(self.search_result_cap);

        Self {
            show_hidden: self.show_hidden,
            columns,
            default_sort,
            default_sort_dir,
            default_filter,
            layout_explore_enabled: self.layout_explore_enabled,
            layout_info_enabled: self.layout_info_enabled,
            layout_explore,
            layout_info,
            search_result_cap,
        }
    }
}

fn default_browse_columns() -> Vec<String> {
    ["name", "size", "date", "type"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn default_browse_sort() -> String { "name".to_string() }
fn default_browse_sort_dir() -> String { "asc".to_string() }
fn default_browse_filter() -> String { "all".to_string() }
fn default_true() -> bool { true }
fn default_browse_layout_open() -> String { "open".to_string() }
fn default_browse_search_result_cap() -> usize { 2000 }

fn normalize_search_result_cap(value: usize) -> usize {
    value.clamp(1, 100_000)
}

fn normalize_browse_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(' ', "_").replace('-', "_")
}

fn normalize_layout_state(value: &str) -> String {
    match normalize_browse_token(value).as_str() {
        "collapsed" | "closed" | "collapse" => "collapsed".to_string(),
        _ => "open".to_string(),
    }
}

fn is_supported_browse_column(value: &str) -> bool {
    matches!(
        value,
        "name"
            | "size"
            | "date"
            | "type"
            | "format"
            | "codec"
            | "sample_rate"
            | "channels"
            | "duration"
            | "artist"
            | "album"
    )
}

fn is_supported_browse_sort(value: &str) -> bool {
    matches!(
        value,
        "name"
            | "size"
            | "date"
            | "type"
            | "format"
            | "codec"
            | "sample_rate"
            | "channels"
            | "duration"
            | "artist"
            | "album"
    )
}

fn is_supported_browse_filter(value: &str) -> bool {
    matches!(
        value,
        "all" | "off" | "audio" | "audio_only" | "flac" | "opus" | "aac" | "mp3"
            | "alac" | "wav" | "wavpack" | "aiff" | "dsf" | "dff" | "dts" | "ac3"
            | "ape" | "lpcm"
    )
}

/// UI-related configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Screen shown when the TUI starts. One of: browse, library, convert, queue, config.
    /// Case-insensitive; unknown values fall back to "browse".
    #[serde(default = "default_initial_screen")]
    pub default_screen: String,
    /// Default action when clicking a preset or "Last used" in the context
    /// menu. "start" = enqueue + start processing (default). "enqueue" =
    /// enqueue only. Holding Shift inverts whichever is set.
    #[serde(default = "default_convert_action")]
    pub convert_default_action: String,
    /// Whether to keep the bit-compare reference after a comparison completes.
    /// false (default) = auto-clear; true = persist until manually cleared.
    #[serde(default)]
    pub compare_keep_reference: bool,
    /// Feature gate for the conversion-actions UI (Output Options row,
    /// :actions / :actions-run / :actions-identity-import). Default OFF while
    /// the feature hardens; config-defined pipelines still apply to
    /// conversions exactly as they do for the CLI.
    #[serde(default)]
    pub show_conversion_actions: bool,
    /// Runtime-selectable TUI theme slug. Unknown values fall back to Tokyo Night.
    #[serde(default = "crate::tui::theme::default_theme_name")]
    pub theme: String,
}

fn default_initial_screen() -> String {
    "browse".to_string()
}

fn default_convert_action() -> String {
    "start".to_string()
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            default_screen: default_initial_screen(),
            show_conversion_actions: false,
            convert_default_action: default_convert_action(),
            compare_keep_reference: false,
            theme: crate::tui::theme::default_theme_name(),
        }
    }
}

/// Writable representation selected for aggregate album metadata operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AggregateMetadataTarget {
    /// Write ordinary tags on the selected audio files.
    IndividualFiles,
    /// Rewrite an external CUE sheet.
    SidecarCue,
    /// Rewrite a CUESHEET tag embedded in an audio image.
    EmbeddedCue,
}

/// Preserve the pre-round-12 preference while making it explicit and ordered.
pub fn default_aggregate_metadata_target_priority() -> Vec<AggregateMetadataTarget> {
    vec![
        AggregateMetadataTarget::SidecarCue,
        AggregateMetadataTarget::EmbeddedCue,
        AggregateMetadataTarget::IndividualFiles,
    ]
}

/// Deduplicate a configured order and append any omitted targets stably.
pub fn normalized_aggregate_metadata_target_priority(
    configured: &[AggregateMetadataTarget],
) -> Vec<AggregateMetadataTarget> {
    let mut normalized = Vec::with_capacity(3);
    for target in configured
        .iter()
        .copied()
        .chain(default_aggregate_metadata_target_priority())
    {
        if !normalized.contains(&target) {
            normalized.push(target);
        }
    }
    if normalized.as_slice() != configured {
        log::debug!(
            "normalized aggregate metadata target priority from {:?} to {:?}",
            configured,
            normalized,
        );
    }
    normalized
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionSettings {
    /// Preferred backend: "ffmpeg" or "sox"
    pub preferred_backend: String,
    /// Number of parallel worker threads
    pub worker_count: usize,
    /// Process priority (-20 to 19)
    pub process_priority: i8,
    /// Calculate ReplayGain after conversion
    pub calculate_replaygain: bool,
    /// Generate CUE files
    pub generate_cue_files: bool,
    /// CUE generation mode: "Always" or "IfMerging"
    pub cue_generation_mode: String,
    /// Write a conversion log file
    pub write_log_file: bool,
    /// Persist queue to disk between sessions
    pub persist_queue: bool,
    /// Default output directory
    pub default_destination: Option<PathBuf>,
    /// Scratch/temp directory for extraction
    pub scratch_directory: Option<PathBuf>,
    /// Maximum percentage of total RAM that scratch/tmpfs staging may reserve (0-90).
    #[serde(default = "default_scratch_memory_limit_percent")]
    pub scratch_memory_limit_percent: u8,
    /// Ephemeral default archive password. Legacy config files may deserialize
    /// this once for migration, but current saves never persist it. Save-state
    /// semantics are tri-state: Some(password) sets or rotates; None with an
    /// archive_password_ref retains; both fields None explicitly clear.
    #[serde(default, skip_serializing)]
    pub archive_password: Option<String>,
    /// Opaque OS secret-store reference for the default archive password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_password_ref: Option<String>,
    /// Default ordered pre/post conversion action pipeline.
    #[serde(default, skip_serializing_if = "crate::convert::pipeline::ActionPipeline::is_empty")]
    pub actions: crate::convert::pipeline::ActionPipeline,
    /// Ordered preference for directory/album metadata write targets.
    #[serde(default = "default_aggregate_metadata_target_priority")]
    pub aggregate_metadata_target_priority: Vec<AggregateMetadataTarget>,
    /// Append content from Lineage.txt to COMMENT tag
    pub append_lineage_to_comment: bool,
}

pub const DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT: u8 = 50;

fn default_scratch_memory_limit_percent() -> u8 {
    DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT
}

fn default_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .saturating_div(2)
        .max(1)
}

impl Default for ConversionSettings {
    fn default() -> Self {
        Self {
            preferred_backend: "ffmpeg".to_string(),
            worker_count: default_worker_count(),
            process_priority: 0,
            calculate_replaygain: true,
            generate_cue_files: false,
            cue_generation_mode: "IfMerging".to_string(),
            write_log_file: false,
            persist_queue: true,
            default_destination: None,
            scratch_directory: None,
            scratch_memory_limit_percent: default_scratch_memory_limit_percent(),
            archive_password: None,
            archive_password_ref: None,
            actions: crate::convert::pipeline::ActionPipeline::default(),
            aggregate_metadata_target_priority: default_aggregate_metadata_target_priority(),
            append_lineage_to_comment: false,
        }
    }
}

impl Default for TonepoetConfig {
    fn default() -> Self {
        Self {
            conversion: ConversionSettings::default(),
            ui: UiConfig::default(),
            performance: PerformanceConfig::default(),
            browsing: BrowsingConfig::default(),
            file_operations: FileOperationsConfig::default(),
            naming: NamingConfig::default(),
        }
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSaveOutcome {
    Durable,
    /// The authoritative config file is durable, but a non-authoritative
    /// cleanup action (currently secret-reference retirement) is deferred.
    DurableWithWarning(String),
    ReplacedButDurabilityUnconfirmed(String),
}

impl ConfigSaveOutcome {
    pub fn warning(&self) -> Option<&str> {
        match self {
            Self::Durable => None,
            Self::DurableWithWarning(message)
            | Self::ReplacedButDurabilityUnconfirmed(message) => Some(message.as_str()),
        }
    }

    pub fn durability_warning(&self) -> Option<&str> {
        match self {
            Self::ReplacedButDurabilityUnconfirmed(message) => Some(message.as_str()),
            Self::Durable | Self::DurableWithWarning(_) => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct StoreFileLock {
    _path: PathBuf,
    _handle: SameFileHandle,
}

const STORE_LOCK_MARKER: &[u8] = b"tonepoet-store-lock-v1\n";
const MAX_STORE_LOCK_MARKER_BYTES: u64 = 4096;

impl StoreFileLock {
    pub(crate) fn acquire_for_path(path: &Path) -> anyhow::Result<(Self, PathBuf)> {
        let target_path = resolve_config_save_target(path)?;
        let parent = target_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let lock = Self::acquire(parent, &target_path)?;
        Ok((lock, target_path))
    }

    fn acquire(parent: &Path, target_path: &Path) -> anyhow::Result<Self> {
        let lock_path = store_lock_path(parent, target_path);
        validate_lock_path_before_open(&lock_path)?;

        let file = match open_store_lock(&lock_path, true) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_lock_path_before_open(&lock_path)?;
                open_store_lock(&lock_path, false).map_err(|error| {
                    anyhow::anyhow!(
                        "open existing store lock '{}' without following links: {error}",
                        lock_path.display()
                    )
                })?
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "create store lock '{}' without following links: {error}",
                    lock_path.display()
                ))
            }
        };

        let mut file = file;
        validate_opened_lock_object(&lock_path, &file)?;
        lock_store_file_bounded(&file, &lock_path)?;

        // Compare the locked object with the current pathname before validating
        // or initializing its marker. The temporary identity handle owns only a
        // clone; the original `file` retains the advisory lock throughout.
        let initial_identity = SameFileHandle::from_file(file.try_clone()?).map_err(|error| {
            anyhow::anyhow!(
                "inspect initial store lock identity '{}': {error}",
                lock_path.display()
            )
        })?;
        validate_locked_path_identity(&lock_path, &initial_identity)?;
        drop(initial_identity);

        if file.metadata()?.len() == 0 {
            if let Err(error) = initialize_store_lock_marker(&mut file, parent, &lock_path) {
                // Never unlink a newly created lock pathname after releasing its
                // file lock. Another process may have opened the same initially
                // empty inode before this process acquired the advisory lock;
                // removing it would split lock authority across two inodes.
                // Retaining the persistent marker is fail-closed: a complete
                // marker remains reusable, while a partial marker is rejected.
                drop(file);
                return Err(anyhow::anyhow!(
                    "{error}; store lock marker '{}' was retained to avoid splitting lock authority",
                    lock_path.display()
                ));
            }
        } else {
            validate_store_lock_marker(&mut file, target_path, &lock_path)?;
        }

        // `same-file` includes file size in Windows identities. Capture the
        // long-lived handle only after marker initialization so its identity
        // matches a fresh pathname handle on every supported platform.
        let handle = SameFileHandle::from_file(file).map_err(|error| {
            anyhow::anyhow!(
                "capture final store lock identity '{}': {error}",
                lock_path.display()
            )
        })?;
        validate_locked_path_identity(&lock_path, &handle)?;

        Ok(Self {
            _path: lock_path,
            _handle: handle,
        })
    }
}

fn lock_store_file_bounded(file: &fs::File, lock_path: &Path) -> anyhow::Result<()> {
    const LOCK_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);
    const LOCK_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);
    let started = std::time::Instant::now();
    loop {
        match fs2::FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let lock_is_held = error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == Some(33); // Windows ERROR_LOCK_VIOLATION
                if !lock_is_held {
                    return Err(anyhow::anyhow!(
                        "lock store sidecar '{}': {error}",
                        lock_path.display()
                    ));
                }
                if started.elapsed() >= LOCK_WAIT_LIMIT {
                    return Err(anyhow::anyhow!(
                        "timed out after {} ms waiting for store update lock: {}",
                        LOCK_WAIT_LIMIT.as_millis(),
                        lock_path.display()
                    ));
                }
                std::thread::sleep(LOCK_RETRY_DELAY);
            }
        }
    }
}

fn open_store_lock(lock_path: &Path, create_new: bool) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(lock_path)
}

fn initialize_store_lock_marker(
    file: &mut fs::File,
    parent: &Path,
    lock_path: &Path,
) -> anyhow::Result<()> {
    file.write_all(STORE_LOCK_MARKER).map_err(|error| {
        anyhow::anyhow!(
            "write store lock provenance marker '{}': {error}",
            lock_path.display()
        )
    })?;
    file.sync_all().map_err(|error| {
        anyhow::anyhow!(
            "sync store lock provenance marker '{}': {error}",
            lock_path.display()
        )
    })?;
    sync_store_lock_parent_dir(parent).map_err(|error| {
        anyhow::anyhow!(
            "sync parent after creating store lock '{}': {error}",
            lock_path.display()
        )
    })
}

fn sync_store_lock_parent_dir(parent: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(message) = TEST_LOCK_MARKER_SYNC_FAILURE.with(|slot| slot.borrow_mut().take()) {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, message));
    }
    sync_parent_dir(parent)
}

fn validate_opened_lock_object(lock_path: &Path, file: &fs::File) -> anyhow::Result<()> {
    let metadata = file.metadata().map_err(|error| {
        anyhow::anyhow!("inspect opened store lock '{}': {error}", lock_path.display())
    })?;
    if !metadata.is_file() {
        return Err(anyhow::anyhow!(
            "store lock path '{}' is not a regular file",
            lock_path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(anyhow::anyhow!(
                "store lock path '{}' has {} hard links; refusing ambiguous lock authority",
                lock_path.display(),
                metadata.nlink()
            ));
        }
    }
    Ok(())
}

fn validate_store_lock_marker(
    file: &mut fs::File,
    target_path: &Path,
    lock_path: &Path,
) -> anyhow::Result<()> {
    let length = file.metadata()?.len();
    if length == 0 || length > MAX_STORE_LOCK_MARKER_BYTES {
        return Err(anyhow::anyhow!(
            "store lock path '{}' has invalid provenance-marker length {}",
            lock_path.display(),
            length
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(length as usize);
    {
        let mut limited = (&mut *file).take(MAX_STORE_LOCK_MARKER_BYTES + 1);
        limited.read_to_end(&mut bytes)?;
    }
    file.seek(SeekFrom::Start(0))?;
    if bytes == STORE_LOCK_MARKER || legacy_store_lock_marker_matches(&bytes, target_path) {
        return Ok(());
    }
    Err(anyhow::anyhow!(
        "store lock path '{}' does not contain a recognized tonepoet lock marker",
        lock_path.display()
    ))
}

fn legacy_store_lock_marker_matches(bytes: &[u8], target_path: &Path) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(line) = text.strip_suffix('\n') else {
        return false;
    };
    let Some((pid, target)) = line
        .strip_prefix("pid=")
        .and_then(|rest| rest.split_once(" target="))
    else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && target == target_path.to_string_lossy().as_ref()
}

fn validate_lock_path_before_open(lock_path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(anyhow::anyhow!(
            "refusing symlinked store lock path '{}'",
            lock_path.display()
        )),
        Ok(metadata) if !metadata.is_file() => Err(anyhow::anyhow!(
            "store lock path '{}' is not a regular file",
            lock_path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow::anyhow!(
            "inspect store lock path '{}': {error}",
            lock_path.display()
        )),
    }
}

fn validate_locked_path_identity(
    lock_path: &Path,
    held: &SameFileHandle,
) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(lock_path).map_err(|error| {
        anyhow::anyhow!(
            "reinspect locked store sidecar '{}': {error}",
            lock_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow::anyhow!(
            "store lock path '{}' became a symlink during acquisition",
            lock_path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(anyhow::anyhow!(
            "store lock path '{}' is not a regular file",
            lock_path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(anyhow::anyhow!(
                "store lock path '{}' has {} hard links; refusing ambiguous lock authority",
                lock_path.display(),
                metadata.nlink()
            ));
        }
    }
    let current = SameFileHandle::from_path(lock_path).map_err(|error| {
        anyhow::anyhow!(
            "open current store lock path '{}' for identity validation: {error}",
            lock_path.display()
        )
    })?;
    if held != &current {
        return Err(anyhow::anyhow!(
            "store lock path '{}' changed identity during acquisition",
            lock_path.display()
        ));
    }
    let final_metadata = fs::symlink_metadata(lock_path).map_err(|error| {
        anyhow::anyhow!(
            "final store lock path validation '{}': {error}",
            lock_path.display()
        )
    })?;
    if final_metadata.file_type().is_symlink() || !final_metadata.is_file() {
        return Err(anyhow::anyhow!(
            "store lock path '{}' changed type during acquisition",
            lock_path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if final_metadata.nlink() != 1 {
            return Err(anyhow::anyhow!(
                "store lock path '{}' changed hard-link count during acquisition",
                lock_path.display()
            ));
        }
    }
    Ok(())
}

fn recover_config_artifacts_locked(target_path: &Path) -> anyhow::Result<()> {
    let parent = target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    recover_stale_config_artifacts(parent, target_path)
}

fn atomic_write_config_locked(
    target_path: &Path,
    content: &[u8],
) -> anyhow::Result<ConfigSaveOutcome> {
    let parent = target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    recover_stale_config_artifacts(parent, target_path)?;

    let file_name = target_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mode = config_file_mode(target_path)?;

    let mut last_create_error = None;
    for attempt in 0..128u32 {
        let tmp_path = parent.join(format!(
            ".{file_name}.tmp.{}.{}.{}",
            std::process::id(),
            stamp,
            attempt
        ));
        match write_and_publish_config_temp(target_path, &tmp_path, content, mode) {
            Ok(outcome) => return Ok(outcome),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_create_error = Some(error);
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(last_create_error
        .unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "could not allocate a unique temporary config path",
            )
        })
        .into())
}

pub(crate) fn resolve_config_save_target(config_path: &Path) -> anyhow::Result<PathBuf> {
    const MAX_CONFIG_SYMLINK_DEPTH: usize = 40;

    let mut current = config_path.to_path_buf();
    let mut visited = std::collections::HashSet::new();

    for depth in 0..=MAX_CONFIG_SYMLINK_DEPTH {
        if !visited.insert(symlink_cycle_key(&current)) {
            return Err(anyhow::anyhow!(
                "configuration path symlink cycle detected at '{}'",
                current.display()
            ));
        }

        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if depth == MAX_CONFIG_SYMLINK_DEPTH {
                    return Err(anyhow::anyhow!(
                        "configuration path '{}' exceeds the maximum symlink depth of {}",
                        config_path.display(),
                        MAX_CONFIG_SYMLINK_DEPTH
                    ));
                }
                let link = fs::read_link(&current).map_err(|error| {
                    anyhow::anyhow!(
                        "read configuration symlink '{}': {error}",
                        current.display()
                    )
                })?;
                current = if link.is_absolute() {
                    link
                } else {
                    current
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(link)
                };
            }
            Ok(_) => return Ok(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(current),
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "inspect configuration path '{}': {error}",
                    current.display()
                ))
            }
        }
    }

    Err(anyhow::anyhow!(
        "configuration path '{}' could not be resolved within the bounded symlink traversal",
        config_path.display()
    ))
}

fn symlink_cycle_key(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut key = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Prefix(prefix) => key.push(prefix.as_os_str()),
            Component::RootDir => key.push(component.as_os_str()),
            Component::ParentDir => key.push(".."),
            Component::Normal(part) => key.push(part),
        }
    }
    if key.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        key
    }
}

#[cfg(unix)]
fn config_file_mode(target_path: &Path) -> std::io::Result<u32> {
    match fs::metadata(target_path) {
        Ok(metadata) if metadata.is_file() => Ok(metadata.permissions().mode() & 0o777),
        Ok(_) => Ok(0o600),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0o600),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn config_file_mode(_target_path: &Path) -> std::io::Result<u32> {
    Ok(0)
}

pub(crate) fn native_os_str_sha256_hex(domain: &[u8], value: &std::ffi::OsStr) -> String {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    digest.update(domain);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        digest.update(value.as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in value.encode_wide() {
            digest.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        digest.update(value.to_string_lossy().as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn store_lock_path(parent: &Path, target_path: &Path) -> PathBuf {
    let file_name = target_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("config.toml"));
    let digest = native_os_str_sha256_hex(b"tonepoet-store-lock-path-v1\0", file_name);
    parent.join(format!(".tonepoet-store-lock-{digest}.save.lock"))
}

pub(crate) fn store_lock_authority_path(path: &Path) -> anyhow::Result<PathBuf> {
    let target_path = resolve_config_save_target(path)?;
    let parent = target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(store_lock_path(parent, &target_path))
}

#[derive(Debug)]
struct ValidatedRecoveryBackup {
    path: PathBuf,
    bytes: Vec<u8>,
}

fn recover_stale_config_artifacts(parent: &Path, target_path: &Path) -> anyhow::Result<()> {
    let file_name = target_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "configuration recovery target '{}' has no UTF-8 file name",
                target_path.display()
            )
        })?;
    let temp_prefix = format!(".{file_name}.tmp.");
    let backup_prefix = format!(".{file_name}.replace-backup.");
    let entries = fs::read_dir(parent).map_err(|error| {
        anyhow::anyhow!(
            "enumerate stale configuration artifacts in '{}': {error}",
            parent.display()
        )
    })?;

    let mut stale_temps = Vec::new();
    let mut backup_paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            anyhow::anyhow!(
                "read stale configuration artifact entry in '{}': {error}",
                parent.display()
            )
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if name.starts_with(&temp_prefix) {
            stale_temps.push(path);
        } else if name.starts_with(&backup_prefix) {
            backup_paths.push(path);
        }
    }

    stale_temps.sort();
    backup_paths.sort();

    let mut first_validation_error = None;
    for path in &stale_temps {
        if let Err(error) =
            validate_recovery_artifact_is_regular(path, "stale temporary configuration file")
        {
            if first_validation_error.is_none() {
                first_validation_error = Some(error);
            }
        }
    }

    let mut backups = Vec::with_capacity(backup_paths.len());
    for path in backup_paths {
        match read_and_validate_recovery_config(
            &path,
            "stale configuration replacement backup",
        ) {
            Ok(bytes) => backups.push(ValidatedRecoveryBackup { path, bytes }),
            Err(error) => {
                if first_validation_error.is_none() {
                    first_validation_error = Some(error);
                }
            }
        }
    }
    if let Some(error) = first_validation_error {
        return Err(error);
    }

    if stale_temps.is_empty() && backups.is_empty() {
        // No recovery artifacts exist, so there is nothing to arbitrate.
        // The target's shape is the saver's concern: an atomic replace onto
        // a non-regular target fails with the honest OS error and cleans up
        // its temp file. Validating the target here would turn that save
        // into a recovery error and leave the rename-failure cleanup path
        // unreachable.
        return Ok(());
    }

    let target_exists = match fs::symlink_metadata(target_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(anyhow::anyhow!(
                    "configuration recovery target '{}' is not a regular file",
                    target_path.display()
                ));
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(anyhow::anyhow!(
                "inspect configuration recovery target '{}': {error}",
                target_path.display()
            ))
        }
    };

    if !target_exists && backups.len() > 1 {
        let candidates = backups
            .iter()
            .map(|backup| format!("'{}'", backup.path.display()))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow::anyhow!(
            "cannot recover missing configuration '{}': multiple valid replacement backups are present: {candidates}",
            target_path.display()
        ));
    }

    if target_exists && !backups.is_empty() {
        read_and_validate_recovery_config(target_path, "published configuration")?;
    }

    if !target_exists {
        if let Some(backup) = backups.first() {
            restore_config_backup_durably(parent, target_path, backup)?;
        }
    }

    let mut cleanup_paths = stale_temps;
    cleanup_paths.extend(backups.into_iter().map(|backup| backup.path));
    let had_cleanup = !cleanup_paths.is_empty();
    if had_cleanup {
        sync_publication_parent_dir(parent).map_err(|error| {
            anyhow::anyhow!(
                "cannot safely remove stale configuration artifacts because parent-directory durability is unavailable: {error}; artifacts were retained and secret reconciliation was not attempted"
            )
        })?;
    }
    for path in cleanup_paths {
        remove_stale_config_artifact(&path).map_err(|error| {
            anyhow::anyhow!(
                "remove stale configuration artifact '{}': {error}",
                path.display()
            )
        })?;
    }
    if had_cleanup {
        sync_publication_parent_dir(parent).map_err(|error| {
            anyhow::anyhow!(
                "stale configuration artifacts were removed, but parent-directory durability could not be confirmed: {error}; secret reconciliation was not attempted"
            )
        })?;
    }
    Ok(())
}

fn validate_recovery_artifact_is_regular(path: &Path, kind: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        anyhow::anyhow!("inspect {kind} '{}': {error}", path.display())
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(anyhow::anyhow!(
            "{kind} '{}' is not a regular file",
            path.display()
        ));
    }
    Ok(())
}

fn read_and_validate_recovery_config(path: &Path, kind: &str) -> anyhow::Result<Vec<u8>> {
    validate_recovery_artifact_is_regular(path, kind)?;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options.open(path).map_err(|error| {
        anyhow::anyhow!("open {kind} '{}' without following links: {error}", path.display())
    })?;
    let metadata = file.metadata().map_err(|error| {
        anyhow::anyhow!("inspect opened {kind} '{}': {error}", path.display())
    })?;
    if !metadata.is_file() {
        return Err(anyhow::anyhow!(
            "{kind} '{}' is not a regular file",
            path.display()
        ));
    }
    let held = SameFileHandle::from_file(file.try_clone()?).map_err(|error| {
        anyhow::anyhow!("inspect {kind} identity '{}': {error}", path.display())
    })?;
    let current = SameFileHandle::from_path(path).map_err(|error| {
        anyhow::anyhow!("reopen {kind} identity '{}': {error}", path.display())
    })?;
    if held != current {
        return Err(anyhow::anyhow!(
            "{kind} '{}' changed identity during validation",
            path.display()
        ));
    }
    validate_recovery_artifact_is_regular(path, kind)?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        anyhow::anyhow!("read {kind} '{}': {error}", path.display())
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        anyhow::anyhow!(
            "{kind} '{}' is not a valid UTF-8 Tonepoet configuration",
            path.display()
        )
    })?;
    toml::from_str::<TonepoetConfig>(text).map_err(|_| {
        anyhow::anyhow!(
            "{kind} '{}' is not a valid Tonepoet configuration",
            path.display()
        )
    })?;
    Ok(bytes)
}

fn restore_config_backup_durably(
    parent: &Path,
    target_path: &Path,
    backup: &ValidatedRecoveryBackup,
) -> anyhow::Result<()> {
    let file_name = target_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    #[cfg(unix)]
    let mode = 0o600;

    let mut last_create_error = None;
    for attempt in 0..128u32 {
        let temporary = parent.join(format!(
            ".{file_name}.tmp.recovery.{}.{}.{}",
            std::process::id(),
            stamp,
            attempt
        ));
        #[cfg(unix)]
        let opened = {
            let mut options = OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .mode(mode & 0o777)
                .open(&temporary)
        };
        #[cfg(not(unix))]
        let opened = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary);
        let mut file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                last_create_error = Some(error);
                continue;
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "create configuration recovery temporary file '{}': {error}",
                    temporary.display()
                ))
            }
        };

        let publish_result = (|| -> anyhow::Result<()> {
            file.write_all(&backup.bytes).map_err(|error| {
                anyhow::anyhow!(
                    "write configuration recovery temporary file '{}': {error}",
                    temporary.display()
                )
            })?;
            file.sync_all().map_err(|error| {
                anyhow::anyhow!(
                    "sync configuration recovery temporary file '{}': {error}",
                    temporary.display()
                )
            })?;
            drop(file);
            fs::rename(&temporary, target_path).map_err(|error| {
                anyhow::anyhow!(
                    "restore configuration '{}' from replacement backup '{}': {error}",
                    target_path.display(),
                    backup.path.display()
                )
            })?;
            sync_publication_parent_dir(parent).map_err(|error| {
                anyhow::anyhow!(
                    "configuration recovery replaced '{}', but parent-directory durability could not be confirmed: {error}; replacement backup '{}' was retained and secret reconciliation was not attempted",
                    target_path.display(),
                    backup.path.display()
                )
            })?;
            Ok(())
        })();

        match publish_result {
            Ok(()) => return Ok(()),
            Err(error) => {
                match fs::symlink_metadata(&temporary) {
                    Ok(_) => {
                        if let Err(cleanup_error) = remove_stale_config_artifact(&temporary) {
                            return Err(anyhow::anyhow!(
                                "{error}; additionally failed to remove recovery temporary file '{}': {cleanup_error}",
                                temporary.display()
                            ));
                        }
                    }
                    Err(metadata_error)
                        if metadata_error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(metadata_error) => {
                        return Err(anyhow::anyhow!(
                            "{error}; additionally failed to inspect recovery temporary file '{}' for cleanup: {metadata_error}",
                            temporary.display()
                        ))
                    }
                }
                return Err(error);
            }
        }
    }

    Err(anyhow::Error::new(last_create_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique configuration recovery temporary file",
        )
    })))
}

fn remove_stale_config_artifact(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    {
        let injected = TEST_STALE_ARTIFACT_REMOVE_FAILURE.with(|slot| {
            let mut slot = slot.borrow_mut();
            match slot.take() {
                Some((target, message)) if target == path => Some(message),
                Some(value) => {
                    *slot = Some(value);
                    None
                }
                None => None,
            }
        });
        if let Some(message) = injected {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, message));
        }
    }
    fs::remove_file(path)
}

fn write_and_publish_config_temp(
    config_path: &Path,
    tmp_path: &Path,
    content: &[u8],
    mode: u32,
) -> std::io::Result<ConfigSaveOutcome> {
    write_and_publish_config_temp_with_sync(
        config_path,
        tmp_path,
        content,
        mode,
        sync_publication_parent_dir,
    )
}

fn write_and_publish_config_temp_with_sync(
    config_path: &Path,
    tmp_path: &Path,
    content: &[u8],
    mode: u32,
    sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<ConfigSaveOutcome> {
    #[cfg(unix)]
    let open_options = {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(mode & 0o777);
        options
    };
    #[cfg(not(unix))]
    let open_options = {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        options
    };

    let mut tmp = match open_options.open(tmp_path) {
        Ok(file) => file,
        Err(error) => return Err(error),
    };

    let mut published = false;
    let result = (|| {
        tmp.write_all(content)?;
        tmp.sync_all()?;
        drop(tmp);
        replace_config_file(tmp_path, config_path)?;
        published = true;
        let parent = config_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        match sync_parent(parent) {
            Ok(()) => Ok(ConfigSaveOutcome::Durable),
            Err(error) => Ok(ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(
                format!("config.toml was written, but parent-directory durability could not be confirmed: {error}"),
            )),
        }
    })();

    if result.is_err() && !published {
        let _ = fs::remove_file(tmp_path);
    }
    result
}

pub(crate) fn replace_config_file(tmp_path: &Path, config_path: &Path) -> std::io::Result<()> {
    // std::fs::rename performs same-filesystem replacement on supported Unix
    // and Windows targets without requiring handwritten platform FFI.
    fs::rename(tmp_path, config_path)
}


#[cfg(unix)]
pub(crate) fn sync_parent_dir(parent: &Path) -> std::io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_parent_dir(_parent: &Path) -> std::io::Result<()> {
    // Some callers use this as a best-available metadata flush rather than a
    // durability classification. Publication paths must instead call
    // `sync_publication_parent_dir`, which reports the missing guarantee.
    Ok(())
}

pub(crate) fn sync_publication_parent_dir(parent: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(message) =
        TEST_CONFIG_PUBLICATION_SYNC_FAILURE.with(|slot| slot.borrow_mut().take())
    {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, message));
    }

    #[cfg(unix)]
    {
        return sync_parent_dir(parent);
    }

    #[cfg(windows)]
    {
        let _ = parent;
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Windows replacement was not performed with write-through semantics",
        ));
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "parent-directory durability is unsupported on this platform",
        ))
    }
}

fn create_restricted_migration_backup(source: &Path, backup: &Path) -> std::io::Result<()> {
    let source_bytes = std::fs::read(source)?;
    let created = if backup.exists() {
        if source_bytes != std::fs::read(backup)? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "existing migration backup '{}' does not match current source '{}'",
                    backup.display(),
                    source.display()
                ),
            ));
        }
        false
    } else {
        match crate::secret_store::atomic_write_private_file(backup, &source_bytes)? {
            crate::secret_store::PrivateFilePublishOutcome::Durable => true,
            crate::secret_store::PrivateFilePublishOutcome::ReplacedButDurabilityUnconfirmed(
                detail,
            ) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!(
                        "cleartext migration backup was replaced but is not durably published: {detail}"
                    ),
                ))
            }
        }
    };
    #[cfg(unix)]
    {
        if let Err(error) = std::fs::set_permissions(
            backup,
            std::fs::Permissions::from_mode(0o600),
        ) {
            let cleanup_error = created
                .then(|| std::fs::remove_file(backup).err())
                .flatten();
            return Err(migration_backup_permission_error(
                backup,
                error,
                cleanup_error,
            ));
        }
        std::fs::File::open(backup)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = created;
    Ok(())
}

#[cfg(unix)]
fn migration_backup_permission_error(
    backup: &Path,
    permission_error: std::io::Error,
    cleanup_error: Option<std::io::Error>,
) -> std::io::Error {
    let message = match cleanup_error {
        Some(cleanup_error) => format!(
            "restrict cleartext migration backup '{}': {permission_error}; additionally failed to remove the newly created unrestricted backup: {cleanup_error}",
            backup.display()
        ),
        None => format!(
            "restrict cleartext migration backup '{}': {permission_error}",
            backup.display()
        ),
    };
    std::io::Error::new(permission_error.kind(), message)
}

fn published_config_secret_references(config_path: &Path) -> anyhow::Result<Vec<String>> {
    match std::fs::read_to_string(config_path) {
        Ok(content) => {
            let value: toml::Value = toml::from_str(&content).map_err(|error| {
                anyhow::anyhow!(
                    "cannot inspect archive-password authority because '{}' is not valid TOML: {error}",
                    config_path.display()
                )
            })?;
            Ok(value
                .get("conversion")
                .and_then(|conversion| conversion.get("archive_password_ref"))
                .and_then(toml::Value::as_str)
                .map(|reference| vec![reference.to_string()])
                .unwrap_or_default())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(anyhow::anyhow!(
            "read '{}' while inspecting archive-password authority: {error}",
            config_path.display()
        )),
    }
}

fn reconcile_config_secret_publication_locked(config_path: &Path) -> anyhow::Result<Vec<String>> {
    let published_references = published_config_secret_references(config_path)?;
    crate::secret_store::reconcile_pending_publication_classified(
        config_path,
        &published_references,
    )
    .map_err(anyhow::Error::new)?;
    Ok(published_references)
}

fn reconcile_config_secret_publication_for_load_locked(
    config_path: &Path,
) -> anyhow::Result<Vec<String>> {
    let published_references = published_config_secret_references(config_path)?;
    match crate::secret_store::reconcile_pending_publication_classified(
        config_path,
        &published_references,
    ) {
        Ok(()) => Ok(published_references),
        Err(error) => {
            let journal = crate::secret_store::pending_publication_path(config_path);
            log::warn!(
                "configuration '{}' loaded without reconciling archive-password publication journal '{}': {}. To recover, repair or restore the config file, then retry; on a headless host use `tonepoet config --retire-secret-journal` to retire only the journal while accepting that unreachable secret entries may remain orphaned",
                config_path.display(),
                journal.display(),
                error
            );
            Ok(published_references)
        }
    }
}

fn config_secret_slot_references(config_path: &Path) -> anyhow::Result<[String; 2]> {
    let durable_path = if config_path.is_absolute() {
        config_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(config_path)
    };
    let durable_key = durable_path.to_string_lossy();
    Ok([
        crate::secret_store::stable_reference("config-a", durable_key.as_ref())
            .map_err(anyhow::Error::new)?,
        crate::secret_store::stable_reference("config-b", durable_key.as_ref())
            .map_err(anyhow::Error::new)?,
    ])
}

fn next_config_secret_reference(
    config_path: &Path,
    current_reference: Option<&str>,
) -> anyhow::Result<String> {
    let [slot_a, slot_b] = config_secret_slot_references(config_path)?;
    Ok(if current_reference == Some(slot_a.as_str()) {
        slot_b
    } else {
        slot_a
    })
}


impl TonepoetConfig {
    /// Load config from the default path (~/.config/tonepoet/config.toml)
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from_path(Self::config_path())
    }

    /// Load config from an explicit path. Cleartext legacy passwords are
    /// migrated while holding the same cross-process lock across authoritative
    /// reads, journal reconciliation, keyring mutation, config publication, and
    /// journal retirement.
    pub fn load_from_path<P: AsRef<Path>>(config_path: P) -> anyhow::Result<Self> {
        let (_lock, target_path) = StoreFileLock::acquire_for_path(config_path.as_ref())?;
        recover_config_artifacts_locked(&target_path)?;
        Self::load_from_locked_path(&target_path)
    }

    fn load_from_locked_path(config_path: &Path) -> anyhow::Result<Self> {
        let _published_references =
            reconcile_config_secret_publication_for_load_locked(config_path)?;
        let secret_reconciliation_deferred =
            crate::secret_store::pending_publication_path(config_path).exists();
        if !config_path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(config_path)?;
        let mut config: TonepoetConfig = toml::from_str(&content)?;

        if let Some(cleartext) = config.conversion.archive_password.clone() {
            if secret_reconciliation_deferred {
                log::warn!(
                    "configuration '{}' retains legacy cleartext archive-password migration input because a prior secret-publication journal could not yet be reconciled",
                    config_path.display()
                );
                return Ok(config);
            }
            let backup = config_path.with_extension("toml.pre-keychain-migration");
            let mut pending_reference = None;
            let reference = if let Some(reference) = config.conversion.archive_password_ref.as_deref() {
                match crate::secret_store::get(reference) {
                    Ok(stored) => {
                        if stored != cleartext {
                            return Err(anyhow::anyhow!(
                                "archive-password migration is ambiguous: config cleartext and secret reference disagree"
                            ));
                        }
                        reference.to_string()
                    }
                    Err(error) if error.is_backend_unavailable() => {
                        log::warn!(
                            "configuration '{}' retains legacy cleartext archive-password migration input because the secret backend is unavailable: {}",
                            config_path.display(),
                            error
                        );
                        return Ok(config);
                    }
                    Err(error) if error.is_not_found() => {
                        match crate::secret_store::set(reference, &cleartext) {
                            Ok(()) => reference.to_string(),
                            Err(store_error) if store_error.is_backend_unavailable() => {
                                log::warn!(
                                    "configuration '{}' retains legacy cleartext archive-password migration input because its missing secret reference could not be repopulated while the backend is unavailable: {}",
                                    config_path.display(),
                                    store_error
                                );
                                return Ok(config);
                            }
                            Err(store_error) => return Err(anyhow::Error::new(store_error)),
                        }
                    }
                    Err(error) => return Err(anyhow::Error::new(error)),
                }
            } else {
                let reference = next_config_secret_reference(config_path, None)?;
                crate::secret_store::begin_pending_publication(
                    config_path,
                    std::slice::from_ref(&reference),
                )
                .map_err(anyhow::Error::msg)?;
                if let Err(store_error) = crate::secret_store::set(&reference, &cleartext) {
                    let unavailable = store_error.is_backend_unavailable();
                    let primary = anyhow::anyhow!(store_error);
                    let cleanup = crate::secret_store::abort_pending_publication(
                        config_path,
                        std::slice::from_ref(&reference),
                    );
                    if unavailable {
                        match cleanup {
                            Ok(()) => log::warn!(
                                "configuration '{}' retains legacy cleartext archive-password migration input because the secret backend is unavailable: {}",
                                config_path.display(),
                                primary
                            ),
                            Err(cleanup_error) => log::warn!(
                                "configuration '{}' retains legacy cleartext archive-password migration input and its pending publication journal because the secret backend is unavailable: {}; cleanup is deferred: {}",
                                config_path.display(),
                                primary,
                                cleanup_error
                            ),
                        }
                        return Ok(config);
                    }
                    match cleanup {
                        Ok(()) => return Err(primary),
                        Err(cleanup_error) => {
                            return Err(anyhow::anyhow!(
                                "{primary}; additionally {cleanup_error}"
                            ))
                        }
                    }
                }
                pending_reference = Some(reference.clone());
                reference
            };

            if let Err(backup_error) = create_restricted_migration_backup(config_path, &backup) {
                let Some(reference) = pending_reference else {
                    return Err(backup_error.into());
                };
                return match crate::secret_store::abort_pending_publication(
                    config_path,
                    std::slice::from_ref(&reference),
                ) {
                    Ok(()) => Err(backup_error.into()),
                    Err(cleanup_error) => Err(anyhow::anyhow!(
                        "could not create restricted archive-password migration backup '{}': {backup_error}; additionally {cleanup_error}",
                        backup.display()
                    )),
                };
            }

            config.conversion.archive_password_ref = Some(reference.clone());
            let mut persisted = config.clone();
            persisted.conversion.archive_password = None;
            let save_outcome = match toml::to_string_pretty(&persisted)
                .map_err(anyhow::Error::new)
                .and_then(|content| atomic_write_config_locked(config_path, content.as_bytes()))
            {
                Ok(outcome) => outcome,
                Err(save_error) => {
                    let Some(reference) = pending_reference else {
                        return Err(save_error);
                    };
                    return match crate::secret_store::abort_pending_publication(
                        config_path,
                        std::slice::from_ref(&reference),
                    ) {
                        Ok(()) => Err(save_error),
                        Err(cleanup_error) => Err(anyhow::anyhow!(
                            "{save_error}; additionally {cleanup_error}"
                        )),
                    };
                }
            };
            match save_outcome {
                ConfigSaveOutcome::Durable => {
                    if pending_reference.is_some() {
                        crate::secret_store::reconcile_pending_publication(
                            config_path,
                            std::slice::from_ref(&reference),
                        )
                        .map_err(anyhow::Error::msg)?;
                    }
                }
                ConfigSaveOutcome::DurableWithWarning(detail) => {
                    return Err(anyhow::anyhow!(
                        "archive-password migration returned an unexpected deferred-cleanup warning: {detail}"
                    ));
                }
                ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(detail) => {
                    let authority = if pending_reference.is_some() {
                        "the pending secret-publication journal was retained for reconciliation"
                    } else {
                        "no new secret reference was created"
                    };
                    return Err(anyhow::anyhow!(
                        "archive-password migration was replaced but is not durably published: {detail}; {authority}"
                    ));
                }
            }
        }
        Ok(config)
    }

    /// Explicitly request removal of the configured archive-password authority
    /// on the next save. A reference-only state retains the existing authority;
    /// clearing both fields requests durable reference removal and retirement.
    pub fn clear_archive_password(&mut self) {
        self.conversion.archive_password = None;
        self.conversion.archive_password_ref = None;
    }

    /// Save config to the default path as an explicit whole-snapshot replacement.
    ///
    /// Callers persisting only selected settings must use [`TonepoetConfig::update`]
    /// instead, so an older snapshot cannot overwrite unrelated on-disk values.
    ///
    /// This convenience API requires confirmed durable publication. If the
    /// destination was replaced but its parent-directory update could not be
    /// confirmed durable, the method returns that structured detail as an
    /// error; callers must not report ordinary save success.
    ///
    /// The entire secret-publication transaction is serialized by one OS lock:
    /// authoritative read, journal reconciliation, keyring mutation, atomic file
    /// publication, and journal retirement all occur under the same ownership.
    pub fn save(&self) -> anyhow::Result<()> {
        require_durable_config_save(self.save_with_outcome()?)
    }

    pub fn save_with_outcome(&self) -> anyhow::Result<ConfigSaveOutcome> {
        self.save_to_path_with_outcome(Self::config_path())
    }

    /// Save config to an explicit path. This exists so UI persistence paths can
    /// be tested against temporary config files without mutating the user's
    /// real configuration. Like `save`, it returns an error after replacement
    /// when publication durability cannot be confirmed.
    pub fn save_to_path<P: AsRef<Path>>(&self, config_path: P) -> anyhow::Result<()> {
        require_durable_config_save(self.save_to_path_with_outcome(config_path)?)
    }

    pub fn save_to_path_with_outcome<P: AsRef<Path>>(
        &self,
        config_path: P,
    ) -> anyhow::Result<ConfigSaveOutcome> {
        self.save_to_path_with_outcome_impl(config_path.as_ref(), || Ok(()))
    }

    /// Atomically update selected settings at the default config path.
    ///
    /// Unlike [`TonepoetConfig::save`], this refreshes the authoritative file
    /// while holding the store lock, applies only the caller's intended change,
    /// and republishes the merged config. UI persistence paths should use this
    /// method so an older in-memory snapshot cannot clobber unrelated settings.
    ///
    /// `apply` runs against both the latest on-disk config and a clone of `self`
    /// before publication. It must therefore be deterministic and free of
    /// externally visible side effects.
    pub(crate) fn update<F>(&mut self, apply: F) -> anyhow::Result<()>
    where
        F: Fn(&mut Self),
    {
        require_durable_config_save(self.update_with_outcome(apply)?)
    }

    pub(crate) fn update_with_outcome<F>(&mut self, apply: F) -> anyhow::Result<ConfigSaveOutcome>
    where
        F: Fn(&mut Self),
    {
        self.update_to_path_with_outcome(Self::config_path(), apply)
    }

    /// Atomically update selected settings at an explicit path.
    pub(crate) fn update_to_path_with_outcome<P, F>(
        &mut self,
        config_path: P,
        apply: F,
    ) -> anyhow::Result<ConfigSaveOutcome>
    where
        P: AsRef<Path>,
        F: Fn(&mut Self),
    {
        let (_lock, target_path) = StoreFileLock::acquire_for_path(config_path.as_ref())?;
        recover_config_artifacts_locked(&target_path)?;

        let mut authoritative = Self::load_from_locked_path(&target_path)?;
        let mut updated_self = self.clone();
        apply(&mut authoritative);
        apply(&mut updated_self);
        let outcome = authoritative.save_to_locked_path_with_outcome_impl(
            &target_path,
            || Ok(()),
        )?;
        *self = updated_self;
        Ok(outcome)
    }

    fn save_to_path_with_outcome_impl<F>(
        &self,
        config_path: &Path,
        after_secret_stored: F,
    ) -> anyhow::Result<ConfigSaveOutcome>
    where
        F: FnMut() -> anyhow::Result<()>,
    {
        let (_lock, target_path) = StoreFileLock::acquire_for_path(config_path)?;
        recover_config_artifacts_locked(&target_path)?;
        self.save_to_locked_path_with_outcome_impl(&target_path, after_secret_stored)
    }

    fn save_to_locked_path_with_outcome_impl<F>(
        &self,
        target_path: &Path,
        mut after_secret_stored: F,
    ) -> anyhow::Result<ConfigSaveOutcome>
    where
        F: FnMut() -> anyhow::Result<()>,
    {
        let published_references = reconcile_config_secret_publication_locked(target_path)?;
        let published_reference = published_references.first().cloned();

        let mut persisted = self.clone();
        let mut newly_stored_reference = None;
        let mut pending_transaction = false;

        match (
            persisted.conversion.archive_password.as_deref(),
            persisted.conversion.archive_password_ref.clone(),
        ) {
            (Some(secret), requested_reference) => {
                let candidate_reference = published_reference
                    .clone()
                    .or(requested_reference);
                let reusable_reference = match candidate_reference.as_deref() {
                    Some(reference) => {
                        let stored = crate::secret_store::get(reference).map_err(|error| {
                            anyhow::anyhow!(
                                "cannot verify existing archive-password reference '{reference}' while saving configuration; no replacement reference was stored: {error}"
                            )
                        })?;
                        (stored == secret).then(|| reference.to_string())
                    }
                    None => None,
                };

                persisted.conversion.archive_password_ref = Some(match reusable_reference {
                    Some(reference) => reference,
                    None => {
                        let reference = next_config_secret_reference(
                            target_path,
                            published_reference.as_deref(),
                        )?;
                        crate::secret_store::begin_pending_publication_with_retirement(
                            target_path,
                            std::slice::from_ref(&reference),
                            &published_references,
                        )
                        .map_err(anyhow::Error::msg)?;
                        pending_transaction = true;
                        if let Err(store_error) = crate::secret_store::set(&reference, secret) {
                            let primary = anyhow::anyhow!(store_error);
                            return match crate::secret_store::abort_pending_publication(
                                target_path,
                                std::slice::from_ref(&reference),
                            ) {
                                Ok(()) => Err(primary),
                                Err(cleanup_error) => Err(anyhow::anyhow!(
                                    "{primary}; additionally {cleanup_error}"
                                )),
                            };
                        }
                        if let Err(hook_error) = after_secret_stored() {
                            return match crate::secret_store::abort_pending_publication(
                                target_path,
                                std::slice::from_ref(&reference),
                            ) {
                                Ok(()) => Err(hook_error),
                                Err(cleanup_error) => Err(anyhow::anyhow!(
                                    "{hook_error}; additionally {cleanup_error}"
                                )),
                            };
                        }
                        newly_stored_reference = Some(reference.clone());
                        reference
                    }
                });
            }
            (None, Some(reference)) => {
                if let Some(published) = published_reference.as_deref() {
                    if published != reference {
                        return Err(anyhow::anyhow!(
                            "cannot retain archive-password reference '{reference}' because the published configuration owns '{published}'"
                        ));
                    }
                }
                persisted.conversion.archive_password_ref = Some(reference);
            }
            (None, None) => {
                // Both fields absent is an explicit clear operation. This is
                // distinct from retaining an existing authority, represented
                // by archive_password_ref = Some(reference).
                persisted.conversion.archive_password_ref = None;
                if !published_references.is_empty() {
                    crate::secret_store::begin_pending_publication_with_retirement(
                        target_path,
                        &[],
                        &published_references,
                    )
                    .map_err(anyhow::Error::msg)?;
                    pending_transaction = true;
                }
            }
        }
        persisted.conversion.archive_password = None;
        let desired_references = persisted
            .conversion
            .archive_password_ref
            .iter()
            .cloned()
            .collect::<Vec<_>>();

        let result = toml::to_string_pretty(&persisted)
            .map_err(anyhow::Error::new)
            .and_then(|content| atomic_write_config_locked(target_path, content.as_bytes()));
        match result {
            Ok(ConfigSaveOutcome::Durable) => {
                if pending_transaction {
                    match crate::secret_store::reconcile_pending_publication_classified(
                        target_path,
                        &desired_references,
                    ) {
                        Ok(()) => {}
                        Err(error) if error.is_backend_unavailable() => {
                            let warning = format!(
                                "configuration is durable, but archive-password cleanup is deferred because the secret backend is unavailable; pending journal '{}': {}",
                                crate::secret_store::pending_publication_path(target_path).display(),
                                error
                            );
                            log::warn!("{warning}");
                            return Ok(ConfigSaveOutcome::DurableWithWarning(warning));
                        }
                        Err(error) => return Err(anyhow::Error::new(error)),
                    }
                }
                Ok(ConfigSaveOutcome::Durable)
            }
            Ok(ConfigSaveOutcome::DurableWithWarning(detail)) => {
                Ok(ConfigSaveOutcome::DurableWithWarning(detail))
            }
            Ok(ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(detail)) => {
                let authority = if pending_transaction {
                    "the pending secret-publication journal was retained for reconciliation"
                } else {
                    "no secret-store mutation was pending"
                };
                Ok(ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(format!(
                    "{detail}; {authority}"
                )))
            }
            Err(save_error) => {
                if let Some(reference) = newly_stored_reference {
                    return match crate::secret_store::abort_pending_publication(
                        target_path,
                        std::slice::from_ref(&reference),
                    ) {
                        Ok(()) => Err(save_error),
                        Err(cleanup_error) => Err(anyhow::anyhow!(
                            "{save_error}; additionally {cleanup_error}"
                        )),
                    };
                }
                if pending_transaction {
                    return match crate::secret_store::reconcile_pending_publication(
                        target_path,
                        &published_references,
                    ) {
                        Ok(()) => Err(save_error),
                        Err(cleanup_error) => Err(anyhow::anyhow!(
                            "{save_error}; additionally failed to reconcile the uncommitted archive-password clear operation: {cleanup_error}"
                        )),
                    };
                }
                Err(save_error)
            }
        }
    }

    /// Get the config file path
    pub fn config_path() -> PathBuf {
        #[cfg(test)]
        if let Some(path) = TEST_CONFIG_PATH_OVERRIDE.with(|slot| slot.borrow().clone()) {
            return path;
        }

        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tonepoet")
            .join("config.toml")
    }
}

fn require_durable_config_save(outcome: ConfigSaveOutcome) -> anyhow::Result<()> {
    match outcome {
        ConfigSaveOutcome::Durable => Ok(()),
        ConfigSaveOutcome::DurableWithWarning(message) => {
            log::warn!("{message}");
            Ok(())
        }
        ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(message) => {
            Err(anyhow::anyhow!(message))
        }
    }
}

#[cfg(test)]
mod theme_config_tests {
    use super::*;

    #[test]
    fn aggregate_metadata_target_priority_defaults_and_normalizes_stably() {
        use AggregateMetadataTarget::{EmbeddedCue, IndividualFiles, SidecarCue};

        assert_eq!(
            default_aggregate_metadata_target_priority(),
            vec![SidecarCue, EmbeddedCue, IndividualFiles],
        );
        assert_eq!(
            normalized_aggregate_metadata_target_priority(&[
                IndividualFiles,
                IndividualFiles,
                EmbeddedCue,
            ]),
            vec![IndividualFiles, EmbeddedCue, SidecarCue],
        );
        assert_eq!(
            normalized_aggregate_metadata_target_priority(&[]),
            default_aggregate_metadata_target_priority(),
        );

        let serialized = toml::to_string_pretty(&TonepoetConfig::default())
            .expect("serialize aggregate metadata priority");
        assert!(serialized.contains("aggregate_metadata_target_priority"));
        assert!(serialized.contains("sidecar-cue"));

        let mut legacy_value: toml::Value =
            toml::from_str(&serialized).expect("parse serialized config as TOML value");
        legacy_value
            .get_mut("conversion")
            .and_then(toml::Value::as_table_mut)
            .expect("conversion table")
            .remove("aggregate_metadata_target_priority");
        let legacy = toml::to_string_pretty(&legacy_value)
            .expect("serialize legacy config without aggregate metadata priority");
        let parsed: TonepoetConfig = toml::from_str(&legacy)
            .expect("deserialize legacy config without aggregate metadata priority");
        assert_eq!(
            parsed.conversion.aggregate_metadata_target_priority,
            default_aggregate_metadata_target_priority(),
        );
    }

    struct ConfigPathOverrideGuard;

    impl ConfigPathOverrideGuard {
        fn install(path: PathBuf) -> Self {
            TEST_CONFIG_PATH_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(path));
            Self
        }
    }

    impl Drop for ConfigPathOverrideGuard {
        fn drop(&mut self) {
            TEST_CONFIG_PATH_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    #[test]
    fn saved_browsing_and_metadata_priority_round_trip_losslessly() {
        use AggregateMetadataTarget::{EmbeddedCue, IndividualFiles, SidecarCue};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let _override = ConfigPathOverrideGuard::install(path.clone());

        let mut initial = TonepoetConfig::default();
        initial.browsing.show_hidden = true;
        initial.save().expect("save initial config");

        let mut configured = TonepoetConfig::load().expect("load initial config");
        configured.browsing.show_hidden = false;
        configured.browsing.layout_explore_enabled = false;
        configured.browsing.layout_explore = "collapsed".to_string();
        configured.conversion.aggregate_metadata_target_priority =
            vec![IndividualFiles, EmbeddedCue, SidecarCue];
        configured.save().expect("save configured values");

        let reloaded = TonepoetConfig::load().expect("reload configured values");
        assert!(!reloaded.browsing.show_hidden);
        assert!(!reloaded.browsing.layout_explore_enabled);
        assert_eq!(reloaded.browsing.layout_explore, "collapsed");
        assert_eq!(
            reloaded.conversion.aggregate_metadata_target_priority,
            vec![IndividualFiles, EmbeddedCue, SidecarCue],
        );

        let once = std::fs::read(&path).expect("first serialized config");
        reloaded.save().expect("repeat idempotent save");
        let twice = std::fs::read(&path).expect("second serialized config");
        assert_eq!(twice, once, "an unchanged save must be byte-idempotent");
    }

    #[test]
    fn narrow_update_does_not_clobber_newer_unrelated_settings() {
        use AggregateMetadataTarget::{EmbeddedCue, IndividualFiles, SidecarCue};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let _override = ConfigPathOverrideGuard::install(path.clone());

        let mut initial = TonepoetConfig::default();
        initial.browsing.show_hidden = true;
        initial.save().expect("save initial config");

        let mut browse_writer = TonepoetConfig::load().expect("load browse writer");
        let mut stale_writer = TonepoetConfig::load().expect("load stale writer");

        browse_writer.browsing.show_hidden = false;
        browse_writer.browsing.layout_explore_enabled = false;
        browse_writer.browsing.layout_explore = "collapsed".to_string();
        browse_writer.conversion.aggregate_metadata_target_priority =
            vec![IndividualFiles, EmbeddedCue, SidecarCue];
        browse_writer.save().expect("save newer user settings");

        stale_writer.performance.browsing.archive_listing_timeout = 45;
        stale_writer
            .update(|latest| {
                latest.performance.browsing.archive_listing_timeout = 45;
            })
            .expect("save unrelated setting from stale snapshot");

        let reloaded = TonepoetConfig::load().expect("reload merged config");
        assert!(!reloaded.browsing.show_hidden);
        assert!(!reloaded.browsing.layout_explore_enabled);
        assert_eq!(reloaded.browsing.layout_explore, "collapsed");
        assert_eq!(
            reloaded.conversion.aggregate_metadata_target_priority,
            vec![IndividualFiles, EmbeddedCue, SidecarCue],
        );
        assert_eq!(reloaded.performance.browsing.archive_listing_timeout, 45);

        let once = std::fs::read(&path).expect("first merged config");
        stale_writer
            .update(|latest| {
                latest.performance.browsing.archive_listing_timeout = 45;
            })
            .expect("repeat identical narrow update");
        let twice = std::fs::read(&path).expect("second merged config");
        assert_eq!(twice, once, "an identical narrow update must be byte-idempotent");
    }

    struct PublicSaveInjectionGuard;

    impl PublicSaveInjectionGuard {
        fn install(path: PathBuf, sync_failure: &str) -> Self {
            TEST_CONFIG_PATH_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(path));
            TEST_CONFIG_PUBLICATION_SYNC_FAILURE.with(|slot| {
                *slot.borrow_mut() = Some(sync_failure.to_string());
            });
            Self
        }
    }

    impl Drop for PublicSaveInjectionGuard {
        fn drop(&mut self) {
            TEST_CONFIG_PATH_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
            TEST_CONFIG_PUBLICATION_SYNC_FAILURE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    struct RecoveryArtifactInjectionGuard;

    impl RecoveryArtifactInjectionGuard {
        fn fail_next_directory_sync(message: &str) -> Self {
            TEST_CONFIG_PUBLICATION_SYNC_FAILURE.with(|slot| {
                *slot.borrow_mut() = Some(message.to_string());
            });
            Self
        }

        fn fail_remove(path: PathBuf, message: &str) -> Self {
            TEST_STALE_ARTIFACT_REMOVE_FAILURE.with(|slot| {
                *slot.borrow_mut() = Some((path, message.to_string()));
            });
            Self
        }
    }

    impl Drop for RecoveryArtifactInjectionGuard {
        fn drop(&mut self) {
            TEST_CONFIG_PUBLICATION_SYNC_FAILURE.with(|slot| *slot.borrow_mut() = None);
            TEST_STALE_ARTIFACT_REMOVE_FAILURE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    fn assert_no_secret_publication_outcome(outcome: ConfigSaveOutcome) {
        #[cfg(unix)]
        assert_eq!(outcome, ConfigSaveOutcome::Durable);

        #[cfg(windows)]
        assert_eq!(
            outcome,
            ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(
                "config.toml was written, but parent-directory durability could not be confirmed: Windows replacement was not performed with write-through semantics; no secret-store mutation was pending".to_string(),
            )
        );

        #[cfg(not(any(unix, windows)))]
        assert_eq!(
            outcome,
            ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(
                "config.toml was written, but parent-directory durability could not be confirmed: parent-directory durability is unsupported on this platform; no secret-store mutation was pending".to_string(),
            )
        );
    }

    #[test]
    fn ui_theme_defaults_to_tokyo_night_when_missing_from_toml() {
        let config: TonepoetConfig = toml::from_str(
            r#"
[conversion]
preferred_backend = "ffmpeg"
worker_count = 2
process_priority = 0
calculate_replaygain = true
generate_cue_files = false
cue_generation_mode = "IfMerging"
write_log_file = false
persist_queue = true
append_lineage_to_comment = false

[ui]
default_screen = "browse"
convert_default_action = "start"
compare_keep_reference = false
# Feature gate for the conversion-actions UI (Output Options row + :actions
# commands) while the feature hardens. Config-defined pipelines still apply.
show_conversion_actions = false
"#,
        )
        .expect("config parses without theme");

        assert_eq!(config.ui.theme, crate::tui::theme::default_theme_slug());
    }


    #[test]
    fn performance_browsing_defaults_when_missing_from_toml() {
        let config: TonepoetConfig = toml::from_str(
            r#"
[conversion]
preferred_backend = "ffmpeg"
worker_count = 2
process_priority = 0
calculate_replaygain = true
generate_cue_files = false
cue_generation_mode = "IfMerging"
write_log_file = false
persist_queue = true
append_lineage_to_comment = false

[ui]
default_screen = "browse"
convert_default_action = "start"
compare_keep_reference = false
# Feature gate for the conversion-actions UI (Output Options row + :actions
# commands) while the feature hardens. Config-defined pipelines still apply.
show_conversion_actions = false
"#,
        )
        .expect("config parses without performance");

        assert_eq!(config.performance.browsing.archive_listing, "auto");
        assert_eq!(config.performance.browsing.archive_listing_timeout, 30);
        assert_eq!(config.file_operations, FileOperationsConfig::default());
    }

    #[test]
    fn file_operation_preferences_round_trip_with_stable_lowercase_values() {
        let mut config = TonepoetConfig::default();
        config.file_operations = FileOperationsConfig {
            verification: tui_file_picker::VerificationMode::Strong,
            status_verbosity: FileOperationStatusVerbosity::Verbose,
            auto_close_progress: true,
            stall_timeout_secs: 12,
        };

        let rendered = toml::to_string(&config).expect("serialize config");
        assert!(rendered.contains("[file_operations]"));
        assert!(rendered.contains("verification = \"strong\""));
        assert!(rendered.contains("status_verbosity = \"verbose\""));
        assert!(rendered.contains("auto_close_progress = true"));
        assert!(rendered.contains("stall_timeout_secs = 12"));

        let reparsed: TonepoetConfig = toml::from_str(&rendered).expect("reparse config");
        assert_eq!(reparsed.file_operations, config.file_operations);
    }


    #[test]
    fn conversion_scratch_memory_limit_defaults_when_missing_from_toml() {
        let config: TonepoetConfig = toml::from_str(
            r#"
[conversion]
preferred_backend = "ffmpeg"
worker_count = 2
process_priority = 0
calculate_replaygain = true
generate_cue_files = false
cue_generation_mode = "IfMerging"
write_log_file = false
persist_queue = true
append_lineage_to_comment = false
"#,
        )
        .expect("config parses without scratch memory limit");

        assert_eq!(
            config.conversion.scratch_memory_limit_percent,
            DEFAULT_SCRATCH_MEMORY_LIMIT_PERCENT
        );
    }

    #[test]
    fn performance_browsing_round_trips_through_toml() {
        let mut config = TonepoetConfig::default();
        config.performance.browsing.archive_listing = "always".to_string();
        config.performance.browsing.archive_listing_timeout = 45;

        let encoded = toml::to_string_pretty(&config).expect("encode config");
        assert!(encoded.contains("[performance.browsing]"));
        assert!(encoded.contains("archive_listing = \"always\""));
        assert!(encoded.contains("archive_listing_timeout = 45"));

        let decoded: TonepoetConfig = toml::from_str(&encoded).expect("decode config");
        assert_eq!(decoded.performance.browsing.archive_listing, "always");
        assert_eq!(decoded.performance.browsing.archive_listing_timeout, 45);
    }

    #[test]
    fn ui_theme_round_trips_through_toml() {
        for palette in crate::tui::theme::palettes() {
            let mut config = TonepoetConfig::default();
            config.ui.theme = palette.slug.to_string();

            let encoded = toml::to_string_pretty(&config).expect("encode config");
            assert!(
                encoded.contains(&format!("theme = \"{}\"", palette.slug)),
                "serialized config must contain theme slug {}",
                palette.slug
            );

            let decoded: TonepoetConfig = toml::from_str(&encoded).expect("decode config");
            assert_eq!(decoded.ui.theme, palette.slug);
        }
    }


    #[test]
    fn browsing_defaults_when_missing_from_toml() {
        let config: TonepoetConfig = toml::from_str(
            r#"
[conversion]
preferred_backend = "ffmpeg"
worker_count = 2
process_priority = 0
calculate_replaygain = true
generate_cue_files = false
cue_generation_mode = "IfMerging"
write_log_file = false
persist_queue = true
append_lineage_to_comment = false
"#,
        )
        .expect("config parses without browsing");

        assert_eq!(config.browsing, BrowsingConfig::default());
    }

    #[test]
    fn browsing_config_normalizes_user_values() {
        let config = BrowsingConfig {
            show_hidden: true,
            columns: vec!["Size".into(), "sample-rate".into(), "size".into()],
            default_sort: "nonsense".into(),
            default_sort_dir: "descending".into(),
            default_filter: "audio only".into(),
            layout_explore_enabled: false,
            layout_info_enabled: true,
            layout_explore: "closed".into(),
            layout_info: "OPEN".into(),
            search_result_cap: 0,
        }
        .normalized();

        assert_eq!(config.columns, vec!["name", "size", "sample_rate"]);
        assert_eq!(config.default_sort, "name");
        assert_eq!(config.default_sort_dir, "desc");
        assert_eq!(config.default_filter, "audio_only");
        assert!(!config.layout_explore_enabled);
        assert!(config.layout_info_enabled);
        assert_eq!(config.layout_explore, "collapsed");
        assert_eq!(config.layout_info, "open");
        assert_eq!(config.search_result_cap, 1);
        assert!(config.show_hidden);
    }

    #[test]
    fn browsing_config_round_trips_through_toml() {
        let mut config = TonepoetConfig::default();
        config.browsing.show_hidden = true;
        config.browsing.columns = vec!["name".into(), "codec".into()];
        config.browsing.default_sort = "date".into();
        config.browsing.default_sort_dir = "desc".into();
        config.browsing.default_filter = "flac".into();
        config.browsing.layout_explore_enabled = true;
        config.browsing.layout_info_enabled = false;
        config.browsing.layout_explore = "collapsed".into();
        config.browsing.search_result_cap = 4096;

        let encoded = toml::to_string_pretty(&config).expect("encode config");
        assert!(encoded.contains("[browsing]"));
        assert!(encoded.contains("show_hidden = true"));
        assert!(encoded.contains("default_sort = \"date\""));
        assert!(encoded.contains("layout_info_enabled = false"));
        assert!(encoded.contains("search_result_cap = 4096"));

        let decoded: TonepoetConfig = toml::from_str(&encoded).expect("decode config");
        assert_eq!(decoded.browsing.normalized(), config.browsing.normalized());
    }

    #[test]
    fn config_save_to_path_uses_atomic_publish_and_round_trips() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("nested").join("config.toml");
        let mut config = TonepoetConfig::default();
        config.conversion.write_log_file = true;
        let outcome = config
            .save_to_path_with_outcome(&path)
            .expect("atomic replacement");
        assert_no_secret_publication_outcome(outcome);

        let encoded = std::fs::read_to_string(&path).expect("saved config");
        assert!(encoded.contains("write_log_file = true"));
        let decoded: TonepoetConfig = toml::from_str(&encoded).expect("saved toml parses");
        assert!(decoded.conversion.write_log_file);
        let temp_leftovers = std::fs::read_dir(path.parent().expect("parent"))
            .expect("list parent")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".config.toml.tmp"))
            .count();
        assert_eq!(temp_leftovers, 0, "successful atomic save must not leave temp files");
    }

    #[cfg(unix)]
    #[test]
    fn config_save_persists_only_secret_reference_and_rehydrates_exact_value() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let mut config = TonepoetConfig::default();
        config.conversion.archive_password = Some("config-secret-value".to_string());

        config.save_to_path(&path).expect("save with secret reference");

        let encoded = std::fs::read_to_string(&path).expect("saved config");
        assert!(!encoded.contains("config-secret-value"));
        let persisted: TonepoetConfig = toml::from_str(&encoded).expect("persisted config");
        let reference = persisted
            .conversion
            .archive_password_ref
            .as_deref()
            .expect("secret reference");
        assert_eq!(
            crate::secret_store::get(reference).expect("rehydrate secret"),
            "config-secret-value"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn default_config_save_explicitly_clears_and_retires_archive_password() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let mut configured = TonepoetConfig::default();
        configured.conversion.archive_password = Some("reset-me".to_string());
        configured.save_to_path(&path).expect("save password before reset");
        let before_reset: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read configured file"),
        )
        .expect("parse configured file");
        let old_reference = before_reset
            .conversion
            .archive_password_ref
            .expect("configured reference");
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);

        TonepoetConfig::default()
            .save_to_path(&path)
            .expect("default save clears password authority");

        let reset: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read reset file"),
        )
        .expect("parse reset file");
        assert_eq!(reset.conversion.archive_password, None);
        assert_eq!(reset.conversion.archive_password_ref, None);
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 0);
        assert_eq!(
            crate::secret_store::get(&old_reference)
                .expect_err("reset must retire old config authority")
                .to_string(),
            format!(
                "archive-password secret store read failed: reference '{}' is unavailable in the opt-in test backend. No cleartext fallback was used",
                old_reference
            )
        );
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn reference_only_save_explicitly_retains_existing_archive_password() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let mut configured = TonepoetConfig::default();
        configured.conversion.archive_password = Some("retain-me".to_string());
        configured.save_to_path(&path).expect("save initial password");
        let persisted: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read initial config"),
        )
        .expect("parse initial config");
        let reference = persisted
            .conversion
            .archive_password_ref
            .expect("initial reference");

        let mut retaining = TonepoetConfig::default();
        retaining.conversion.archive_password_ref = Some(reference.clone());
        retaining.conversion.write_log_file = true;
        retaining.save_to_path(&path).expect("retain reference-only authority");

        let retained: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read retained config"),
        )
        .expect("parse retained config");
        assert_eq!(retained.conversion.archive_password, None);
        assert_eq!(retained.conversion.archive_password_ref, Some(reference.clone()));
        assert!(retained.conversion.write_log_file);
        assert_eq!(
            crate::secret_store::get(&reference).expect("retained authority remains"),
            "retain-me"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn repeated_saves_and_rotation_are_state_idempotent() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let mut config = TonepoetConfig::default();
        config.conversion.archive_password = Some("first-secret".to_string());

        config.save_to_path(&path).expect("first save");
        let first_persisted: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read first config"),
        )
        .expect("parse first config");
        let first_reference = first_persisted
            .conversion
            .archive_password_ref
            .clone()
            .expect("first reference");

        config.save_to_path(&path).expect("repeat same in-memory save");
        let repeated_persisted: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read repeated config"),
        )
        .expect("parse repeated config");
        assert_eq!(
            repeated_persisted.conversion.archive_password_ref,
            Some(first_reference.clone())
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
        assert_eq!(
            crate::secret_store::get(&first_reference).expect("first authority"),
            "first-secret"
        );

        config.conversion.archive_password = Some("rotated-secret".to_string());
        config.save_to_path(&path).expect("rotate password");
        let rotated_persisted: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read rotated config"),
        )
        .expect("parse rotated config");
        let rotated_reference = rotated_persisted
            .conversion
            .archive_password_ref
            .clone()
            .expect("rotated reference");
        assert_ne!(rotated_reference, first_reference);
        assert_eq!(
            crate::secret_store::get(&rotated_reference).expect("rotated authority"),
            "rotated-secret"
        );
        assert_eq!(
            crate::secret_store::get(&first_reference)
                .expect_err("superseded first authority must be absent")
                .to_string(),
            format!(
                "archive-password secret store read failed: reference '{}' is unavailable in the opt-in test backend. No cleartext fallback was used",
                first_reference
            )
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);

        config.conversion.archive_password = Some("failed-rotation".to_string());
        let error = config
            .save_to_path_with_outcome_impl(&path, || {
                Err(anyhow::anyhow!("synthetic failure before config publication"))
            })
            .expect_err("failed rotation must preserve old authority");
        assert_eq!(
            error.to_string(),
            "synthetic failure before config publication"
        );
        let after_failure: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read config after failed rotation"),
        )
        .expect("parse config after failed rotation");
        assert_eq!(
            after_failure.conversion.archive_password_ref,
            Some(rotated_reference.clone())
        );
        assert_eq!(
            crate::secret_store::get(&rotated_reference).expect("old authority retained"),
            "rotated-secret"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn rotation_retires_a_pre_slot_config_reference_from_v3() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let old_reference = crate::secret_store::allocate_reference();
        crate::secret_store::set(&old_reference, "v3-secret").expect("store v3 authority");
        let mut old_persisted = TonepoetConfig::default();
        old_persisted.conversion.archive_password_ref = Some(old_reference.clone());
        std::fs::write(
            &path,
            toml::to_string_pretty(&old_persisted).expect("serialize v3 config"),
        )
        .expect("write v3 config");

        let mut replacement = TonepoetConfig::default();
        replacement.conversion.archive_password = Some("v4-secret".to_string());
        replacement
            .save_to_path(&path)
            .expect("rotate v3 config authority");

        let persisted: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read rotated config"),
        )
        .expect("parse rotated config");
        let new_reference = persisted
            .conversion
            .archive_password_ref
            .expect("new config reference");
        assert_ne!(new_reference, old_reference);
        assert_eq!(
            crate::secret_store::get(&new_reference).expect("new authority"),
            "v4-secret"
        );
        assert_eq!(
            crate::secret_store::get(&old_reference)
                .expect_err("pre-v4 authority must be retired")
                .to_string(),
            format!(
                "archive-password secret store read failed: reference '{}' is unavailable in the opt-in test backend. No cleartext fallback was used",
                old_reference
            )
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn competing_config_writer_threads_wait_and_serialize_the_whole_transaction() {
        use std::sync::{mpsc, Arc, Barrier};

        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let stored = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));

        let first_path = path.clone();
        let first_stored = Arc::clone(&stored);
        let first_release = Arc::clone(&release);
        let first = std::thread::spawn(move || {
            let mut config = TonepoetConfig::default();
            config.conversion.archive_password = Some("writer-a".to_string());
            config.save_to_path_with_outcome_impl(&first_path, || {
                first_stored.wait();
                first_release.wait();
                Ok(())
            })
        });

        stored.wait();
        let second_path = path.clone();
        let (second_done_tx, second_done_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            let mut config = TonepoetConfig::default();
            config.conversion.archive_password = Some("writer-b".to_string());
            let result = config.save_to_path(&second_path);
            second_done_tx.send(()).expect("signal second completion");
            result
        });
        assert!(
            second_done_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "second writer must wait while the first owns the lock"
        );

        release.wait();
        assert_eq!(
            first.join().expect("first writer thread").expect("first writer"),
            ConfigSaveOutcome::Durable
        );
        second_done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("second writer completes after release");
        second
            .join()
            .expect("second writer thread")
            .expect("second writer");

        let persisted: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read final config"),
        )
        .expect("parse final config");
        let final_reference = persisted
            .conversion
            .archive_password_ref
            .expect("final reference");
        let [slot_a, slot_b] = config_secret_slot_references(&path).expect("config slots");
        assert_eq!(final_reference, slot_b);
        assert_eq!(
            crate::secret_store::get(&slot_a)
                .expect_err("writer A slot must be retired")
                .to_string(),
            format!(
                "archive-password secret store read failed: reference '{}' is unavailable in the opt-in test backend. No cleartext fallback was used",
                slot_a
            )
        );
        assert_eq!(
            crate::secret_store::get(&final_reference).expect("final authority"),
            "writer-b"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
    }

    #[test]
    #[ignore]
    fn store_lock_subprocess_helper() {
        let target = PathBuf::from(
            std::env::var_os("TONEPOET_TEST_LOCK_TARGET")
                .expect("subprocess lock target"),
        );
        let ready = PathBuf::from(
            std::env::var_os("TONEPOET_TEST_LOCK_READY")
                .expect("subprocess ready marker"),
        );
        let (_lock, resolved) = StoreFileLock::acquire_for_path(&target)
            .expect("subprocess acquires store lock");
        std::fs::write(&ready, resolved.to_string_lossy().as_bytes())
            .expect("publish subprocess ready marker");
        loop {
            std::thread::park();
        }
    }

    #[test]
    #[ignore]
    fn store_lock_empty_sidecar_subprocess_helper() {
        let target = PathBuf::from(
            std::env::var_os("TONEPOET_TEST_LOCK_TARGET")
                .expect("subprocess lock target"),
        );
        let ready = PathBuf::from(
            std::env::var_os("TONEPOET_TEST_LOCK_READY")
                .expect("subprocess ready marker"),
        );
        let parent = target.parent().expect("lock target parent");
        let lock_path = store_lock_path(parent, &target);
        let file = open_store_lock(&lock_path, true).expect("create empty lock sidecar");
        fs2::FileExt::lock_exclusive(&file).expect("lock empty sidecar");
        std::fs::write(&ready, lock_path.to_string_lossy().as_bytes())
            .expect("publish empty-sidecar readiness");
        loop {
            std::thread::park();
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn store_lock_excludes_an_independent_process_and_releases_after_process_death() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("config.toml");
        let ready = temp.path().join("child-ready");
        let executable = std::env::current_exe().expect("current test executable");
        let mut child = std::process::Command::new(executable)
            .arg("--ignored")
            .arg("--exact")
            .arg("config::theme_config_tests::store_lock_subprocess_helper")
            .env("TONEPOET_TEST_LOCK_TARGET", &target)
            .env("TONEPOET_TEST_LOCK_READY", &ready)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn independent lock holder");

        for _ in 0..500 {
            if ready.exists() {
                break;
            }
            if let Some(status) = child.try_wait().expect("poll lock holder") {
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut stderr| {
                        use std::io::Read;
                        let mut bytes = Vec::new();
                        stderr.read_to_end(&mut bytes).expect("read child stderr");
                        String::from_utf8_lossy(&bytes).into_owned()
                    })
                    .unwrap_or_default();
                panic!("lock-holder subprocess exited early with {status}: {stderr}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !ready.exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("independent lock holder did not publish readiness");
        }

        let lock_path = store_lock_path(temp.path(), &target);
        let blocked = StoreFileLock::acquire_for_path(&target)
            .expect_err("independent holder must exclude this process")
            .to_string();

        child.kill().expect("terminate lock holder abnormally");
        let status = child.wait().expect("reap lock holder");
        assert!(!status.success());
        assert_eq!(
            blocked,
            format!(
                "timed out after 2000 ms waiting for store update lock: {}",
                lock_path.display()
            )
        );
        let (_recovered_lock, resolved) = StoreFileLock::acquire_for_path(&target)
            .expect("OS releases lock after holder process dies");
        assert_eq!(resolved, target);
        assert!(std::fs::symlink_metadata(&lock_path)
            .expect("lock sidecar metadata")
            .is_file());
    }

    #[test]
    fn store_lock_marker_is_created_once_and_existing_marker_is_not_rewritten() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("config.toml");
        let lock_path = store_lock_path(temp.path(), &target);

        {
            let (_lock, resolved) = StoreFileLock::acquire_for_path(&target)
                .expect("create first store lock marker");
            assert_eq!(resolved, target);
        }
        assert_eq!(
            std::fs::read(&lock_path).expect("read first marker"),
            STORE_LOCK_MARKER
        );

        let legacy = format!("pid=42 target={}\n", target.display());
        std::fs::write(&lock_path, legacy.as_bytes()).expect("install v4 legacy marker");
        {
            let (_lock, resolved) = StoreFileLock::acquire_for_path(&target)
                .expect("accept legacy marker without migration write");
            assert_eq!(resolved, target);
        }
        assert_eq!(
            std::fs::read(&lock_path).expect("read unchanged legacy marker"),
            legacy.as_bytes()
        );
    }

    #[test]
    fn store_lock_adopts_an_unlocked_empty_sidecar_after_creator_death() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("config.toml");
        let lock_path = store_lock_path(temp.path(), &target);
        std::fs::File::create(&lock_path).expect("create abandoned empty sidecar");

        let (_lock, resolved) = StoreFileLock::acquire_for_path(&target)
            .expect("locked process initializes abandoned empty sidecar");

        assert_eq!(resolved, target);
        assert_eq!(
            std::fs::read(&lock_path).expect("read initialized marker"),
            STORE_LOCK_MARKER
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn store_lock_empty_creator_window_is_bounded_and_recoverable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("config.toml");
        let ready = temp.path().join("empty-child-ready");
        let lock_path = store_lock_path(temp.path(), &target);
        let executable = std::env::current_exe().expect("current test executable");
        let mut child = std::process::Command::new(executable)
            .arg("--ignored")
            .arg("--exact")
            .arg("config::theme_config_tests::store_lock_empty_sidecar_subprocess_helper")
            .env("TONEPOET_TEST_LOCK_TARGET", &target)
            .env("TONEPOET_TEST_LOCK_READY", &ready)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn empty-sidecar lock holder");

        for _ in 0..500 {
            if ready.exists() {
                break;
            }
            if let Some(status) = child.try_wait().expect("poll empty-sidecar holder") {
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut stderr| {
                        use std::io::Read;
                        let mut bytes = Vec::new();
                        stderr.read_to_end(&mut bytes).expect("read child stderr");
                        String::from_utf8_lossy(&bytes).into_owned()
                    })
                    .unwrap_or_default();
                panic!("empty-sidecar holder exited early with {status}: {stderr}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !ready.exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("empty-sidecar holder did not publish readiness");
        }

        let blocked = StoreFileLock::acquire_for_path(&target)
            .expect_err("empty but locked sidecar must wait, not fail marker validation")
            .to_string();
        assert_eq!(
            blocked,
            format!(
                "timed out after 2000 ms waiting for store update lock: {}",
                lock_path.display()
            )
        );

        child.kill().expect("terminate empty-sidecar holder");
        let status = child.wait().expect("reap empty-sidecar holder");
        assert!(!status.success());
        let (_lock, resolved) = StoreFileLock::acquire_for_path(&target)
            .expect("next lock owner initializes empty sidecar after creator death");
        assert_eq!(resolved, target);
        assert_eq!(
            std::fs::read(&lock_path).expect("read recovered marker"),
            STORE_LOCK_MARKER
        );
    }

    #[test]
    fn failed_new_lock_marker_initialization_retains_one_lock_inode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("config.toml");
        let lock_path = store_lock_path(temp.path(), &target);
        TEST_LOCK_MARKER_SYNC_FAILURE.with(|slot| {
            *slot.borrow_mut() = Some("synthetic lock-parent sync failure".to_string());
        });

        let error = StoreFileLock::acquire_for_path(&target)
            .expect_err("marker parent-sync failure must abort acquisition")
            .to_string();

        assert_eq!(
            error,
            format!(
                "sync parent after creating store lock '{}': synthetic lock-parent sync failure; store lock marker '{}' was retained to avoid splitting lock authority",
                lock_path.display(),
                lock_path.display()
            )
        );
        assert_eq!(
            std::fs::read(&lock_path).expect("retained marker"),
            STORE_LOCK_MARKER
        );

        let (_lock, resolved) = StoreFileLock::acquire_for_path(&target)
            .expect("retained complete marker remains the sole lock authority");
        assert_eq!(resolved, target);
        assert_eq!(
            std::fs::read(&lock_path).expect("unchanged retained marker"),
            STORE_LOCK_MARKER
        );
    }

    #[test]
    fn store_lock_rejects_unrecognized_regular_file_without_rewriting_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("config.toml");
        let lock_path = store_lock_path(temp.path(), &target);
        std::fs::write(&lock_path, b"unrelated regular-file bytes")
            .expect("write unrelated lock-path object");

        assert_eq!(
            StoreFileLock::acquire_for_path(&target)
                .expect_err("unrecognized marker must fail closed")
                .to_string(),
            format!(
                "store lock path '{}' does not contain a recognized tonepoet lock marker",
                lock_path.display()
            )
        );
        assert_eq!(
            std::fs::read(&lock_path).expect("read untouched unrelated file"),
            b"unrelated regular-file bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn store_lock_rejects_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("config.toml");
        let lock_path = store_lock_path(temp.path(), &target);
        let victim = temp.path().join("victim.txt");
        std::fs::write(&victim, b"authoritative victim bytes").expect("write victim");
        symlink(&victim, &lock_path).expect("create hostile lock symlink");

        assert_eq!(
            StoreFileLock::acquire_for_path(&target)
                .expect_err("symlinked lock path must fail closed")
                .to_string(),
            format!("refusing symlinked store lock path '{}'", lock_path.display())
        );
        assert_eq!(
            std::fs::read(&victim).expect("read untouched victim"),
            b"authoritative victim bytes"
        );
        assert!(std::fs::symlink_metadata(&lock_path)
            .expect("lock symlink remains")
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn store_lock_rejects_hard_link_without_touching_its_other_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("config.toml");
        let lock_path = store_lock_path(temp.path(), &target);
        let victim = temp.path().join("victim.txt");
        std::fs::write(&victim, b"hard-linked victim bytes").expect("write victim");
        std::fs::hard_link(&victim, &lock_path).expect("create hostile lock hard link");

        assert_eq!(
            StoreFileLock::acquire_for_path(&target)
                .expect_err("multiply linked lock path must fail closed")
                .to_string(),
            format!(
                "store lock path '{}' has 2 hard links; refusing ambiguous lock authority",
                lock_path.display()
            )
        );
        assert_eq!(
            std::fs::read(&victim).expect("read untouched hard-link target"),
            b"hard-linked victim bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_reset_publishes_durably_when_secret_cleanup_is_temporarily_unavailable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        {
            let _backend = crate::secret_store::enable_insecure_test_backend();
            let mut configured = TonepoetConfig::default();
            configured.conversion.archive_password = Some("reset-authority".to_string());
            configured.save_to_path(&path).expect("publish configured authority");
        }

        let unavailable = crate::secret_store::enable_unavailable_test_backend();
        let outcome = TonepoetConfig::default()
            .save_to_path_with_outcome(&path)
            .expect("reset publication must not depend on immediate secret cleanup");
        let warning = match outcome {
            ConfigSaveOutcome::DurableWithWarning(warning) => warning,
            other => panic!("expected deferred-cleanup warning, got {other:?}"),
        };
        assert!(warning.contains("configuration is durable"));
        assert!(warning.contains("injected unavailable secret backend"));
        let reset: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("read durable reset"),
        )
        .expect("parse durable reset");
        assert_eq!(reset.conversion.archive_password, None);
        assert_eq!(reset.conversion.archive_password_ref, None);
        assert!(crate::secret_store::pending_publication_path(&path).exists());
        drop(unavailable);

        let _backend = crate::secret_store::enable_insecure_test_backend();
        TonepoetConfig::load_from_path(&path).expect("later startup retires deferred cleanup");
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
    }

    #[test]
    fn config_save_with_unavailable_existing_reference_fails_without_storing_a_replacement() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let mut config = TonepoetConfig::default();
        config.conversion.archive_password = Some("rehydrated-secret".to_string());
        config.conversion.archive_password_ref =
            Some("archive-password:missing-save-reference".to_string());

        let error = config
            .save_to_path(&path)
            .expect_err("an unavailable existing reference must fail closed");

        assert_eq!(
            error.to_string(),
            "cannot verify existing archive-password reference 'archive-password:missing-save-reference' while saving configuration; no replacement reference was stored: archive-password secret store read failed: reference 'archive-password:missing-save-reference' is unavailable in the opt-in test backend. No cleartext fallback was used"
        );
        assert!(!path.exists(), "failed validation must not publish configuration");
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn config_save_failure_removes_unpublished_secret_reference() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        std::fs::create_dir(&path).expect("directory target blocks atomic rename");
        let mut config = TonepoetConfig::default();
        config.conversion.archive_password = Some("must-not-be-orphaned".to_string());

        config
            .save_to_path(&path)
            .expect_err("publishing over a directory must fail");

        assert_eq!(
            crate::secret_store::insecure_test_secret_count(),
            0,
            "a failed config publish must not orphan its newly created secret"
        );
    }

    #[test]
    fn config_load_keeps_unavailable_secret_reference_lazy() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let mut config = TonepoetConfig::default();
        config.conversion.archive_password_ref =
            Some("archive-password:missing-config-reference".to_string());
        std::fs::write(
            &path,
            toml::to_string_pretty(&config).expect("serialize referenced config"),
        )
        .expect("write referenced config");

        let loaded = TonepoetConfig::load_from_path(&path)
            .expect("backend unavailability must not brick startup");
        assert_eq!(loaded.conversion.archive_password, None);
        assert_eq!(
            loaded.conversion.archive_password_ref.as_deref(),
            Some("archive-password:missing-config-reference")
        );
    }

    #[test]
    fn config_load_degrades_on_malformed_pending_secret_journal_and_retains_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let config = TonepoetConfig::default();
        std::fs::write(
            &path,
            toml::to_string_pretty(&config).expect("serialize config"),
        )
        .expect("write config");
        let journal = crate::secret_store::pending_publication_path(&path);
        std::fs::write(&journal, b"{malformed").expect("write malformed journal");

        let loaded = TonepoetConfig::load_from_path(&path)
            .expect("load must degrade rather than brick startup");

        assert_eq!(loaded.conversion.archive_password, None);
        assert_eq!(loaded.conversion.archive_password_ref, None);
        assert_eq!(std::fs::read(&journal).expect("journal retained"), b"{malformed");
    }

    #[test]
    fn config_load_defers_cleartext_migration_when_backend_is_unavailable_then_recovers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let baseline = toml::to_string_pretty(&TonepoetConfig::default())
            .expect("serialize baseline config");
        let legacy = baseline.replacen(
            "[conversion]\n",
            "[conversion]\narchive_password = \"legacy-secret\"\n",
            1,
        );
        std::fs::write(&path, &legacy).expect("write legacy config");

        let unavailable = crate::secret_store::enable_unavailable_test_backend();
        let degraded = TonepoetConfig::load_from_path(&path)
            .expect("backend unavailability must not brick config load");
        assert_eq!(
            degraded.conversion.archive_password.as_deref(),
            Some("legacy-secret")
        );
        assert_eq!(degraded.conversion.archive_password_ref, None);
        assert_eq!(std::fs::read_to_string(&path).expect("legacy retained"), legacy);
        assert!(
            crate::secret_store::pending_publication_path(&path).exists(),
            "failed cleanup keeps the journal for later orphan reconciliation"
        );
        drop(unavailable);

        let _backend = crate::secret_store::enable_insecure_test_backend();
        let migrated = TonepoetConfig::load_from_path(&path)
            .expect("later load should reconcile and complete one-shot migration");
        assert_eq!(
            migrated.conversion.archive_password.as_deref(),
            Some("legacy-secret")
        );
        let reference = migrated
            .conversion
            .archive_password_ref
            .as_deref()
            .expect("migration publishes opaque reference");
        assert_eq!(crate::secret_store::get(reference).as_deref(), Ok("legacy-secret"));
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
        let persisted = std::fs::read_to_string(&path).expect("read migrated config");
        assert!(!persisted.contains("archive_password = \"legacy-secret\""));
        assert!(persisted.contains(reference));
    }

    #[test]
    fn config_load_repopulates_missing_published_reference_from_surviving_cleartext() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let reference = crate::secret_store::stable_reference("config-a", "missing-authority")
            .expect("stable reference");
        let mut config = TonepoetConfig::default();
        config.conversion.archive_password_ref = Some(reference.clone());
        let baseline = toml::to_string_pretty(&config).expect("serialize referenced config");
        let legacy = baseline.replacen(
            "[conversion]\n",
            "[conversion]\narchive_password = \"surviving-cleartext\"\n",
            1,
        );
        std::fs::write(&path, legacy).expect("write recoverable legacy config");

        let loaded = TonepoetConfig::load_from_path(&path)
            .expect("surviving cleartext should repopulate its missing published authority");

        assert_eq!(loaded.conversion.archive_password.as_deref(), Some("surviving-cleartext"));
        assert_eq!(loaded.conversion.archive_password_ref.as_deref(), Some(reference.as_str()));
        assert_eq!(crate::secret_store::get(&reference).as_deref(), Ok("surviving-cleartext"));
        let persisted = std::fs::read_to_string(&path).expect("read migrated config");
        assert!(!persisted.contains("archive_password = \"surviving-cleartext\""));
        assert!(persisted.contains(&reference));
    }

    #[test]
    fn config_load_rejects_disagreeing_legacy_cleartext_and_reference() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let reference = crate::secret_store::store("referenced-secret").expect("store reference");
        let mut config = TonepoetConfig::default();
        config.conversion.archive_password_ref = Some(reference);
        let baseline = toml::to_string_pretty(&config).expect("serialize referenced config");
        let legacy = baseline.replacen(
            "[conversion]\n",
            "[conversion]\narchive_password = \"conflicting-cleartext\"\n",
            1,
        );
        std::fs::write(&path, &legacy).expect("write conflicting legacy config");

        let error = TonepoetConfig::load_from_path(&path)
            .expect_err("conflicting persisted password authorities must fail closed");

        assert_eq!(
            error.to_string(),
            "archive-password migration is ambiguous: config cleartext and secret reference disagree"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("original retained"), legacy);
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn migration_backup_permission_error_reports_cleanup_failure() {
        let error = migration_backup_permission_error(
            Path::new("/tmp/config.toml.pre-keychain-migration"),
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "chmod denied"),
            Some(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "unlink denied",
            )),
        );

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            error.to_string(),
            "restrict cleartext migration backup '/tmp/config.toml.pre-keychain-migration': chmod denied; additionally failed to remove the newly created unrestricted backup: unlink denied"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_migration_rejects_a_stale_backup_and_removes_new_secret_reference() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let baseline = toml::to_string_pretty(&TonepoetConfig::default())
            .expect("serialize baseline config");
        let legacy = baseline.replacen(
            "[conversion]\n",
            "[conversion]\narchive_password = \"current-secret\"\n",
            1,
        );
        std::fs::write(&path, &legacy).expect("legacy config");
        std::fs::write(
            temp.path().join("config.toml.pre-keychain-migration"),
            "stale backup bytes",
        )
        .expect("stale backup");

        let error = TonepoetConfig::load_from_path(&path)
            .expect_err("stale backup must block migration");

        assert!(
            error.to_string().contains("does not match current source"),
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("source retained"), legacy);
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn matching_existing_config_migration_backup_is_restricted_to_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("config.toml");
        let backup = temp.path().join("config.toml.pre-keychain-migration");
        let bytes = b"archive_password = \"legacy-secret\"\n";
        std::fs::write(&source, bytes).expect("source");
        std::fs::write(&backup, bytes).expect("matching backup");
        std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o644))
            .expect("set permissive backup mode");

        create_restricted_migration_backup(&source, &backup)
            .expect("matching backup should be accepted and restricted");

        assert_eq!(std::fs::read(&backup).expect("backup bytes"), bytes);
        assert_eq!(
            std::fs::metadata(&backup)
                .expect("backup metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_config_replacement_backup_is_restored_before_secret_reconciliation() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let backup = temp
            .path()
            .join(".config.toml.replace-backup.crashed.1");
        let reference = crate::secret_store::allocate_reference();
        crate::secret_store::set(&reference, "restored-authority")
            .expect("store referenced secret");
        crate::secret_store::begin_pending_publication(
            &path,
            std::slice::from_ref(&reference),
        )
        .expect("journal interrupted publication");
        let mut persisted = TonepoetConfig::default();
        persisted.conversion.archive_password_ref = Some(reference.clone());
        std::fs::write(
            &backup,
            toml::to_string_pretty(&persisted).expect("serialize replacement backup"),
        )
        .expect("write replacement backup");

        let loaded = TonepoetConfig::load_from_path(&path)
            .expect("restore authoritative config before reconciling its secret");

        assert_eq!(loaded.conversion.archive_password, None);
        assert_eq!(
            loaded.conversion.archive_password_ref.as_deref(),
            Some(reference.as_str())
        );
        assert_eq!(
            crate::secret_store::get(&reference).expect("restored secret retained"),
            "restored-authority"
        );
        assert!(path.exists());
        assert!(!backup.exists());
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn missing_config_reconciles_an_unpublished_secret_before_returning_defaults() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let orphan = crate::secret_store::allocate_reference();
        crate::secret_store::begin_pending_publication(
            &path,
            std::slice::from_ref(&orphan),
        )
        .expect("journal interrupted initial save");
        crate::secret_store::set(&orphan, "unpublished-secret")
            .expect("store simulated orphan");

        let loaded = TonepoetConfig::load_from_path(&path)
            .expect("missing config should reconcile and return defaults");

        assert_eq!(loaded.conversion.archive_password, None);
        assert_eq!(loaded.conversion.archive_password_ref, None);
        assert!(crate::secret_store::get(&orphan).is_err());
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 0);
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn config_migration_reconciles_crash_after_secret_store_before_reference_publish() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let baseline = toml::to_string_pretty(&TonepoetConfig::default())
            .expect("serialize baseline config");
        let legacy = baseline.replacen(
            "[conversion]\n",
            "[conversion]\narchive_password = \"legacy-after-crash\"\n",
            1,
        );
        std::fs::write(&path, &legacy).expect("write legacy config");
        let orphan = crate::secret_store::allocate_reference();
        crate::secret_store::begin_pending_publication(
            &path,
            std::slice::from_ref(&orphan),
        )
        .expect("journal simulated interrupted migration");
        crate::secret_store::set(&orphan, "legacy-after-crash")
            .expect("store simulated orphan");

        let loaded = TonepoetConfig::load_from_path(&path)
            .expect("reconcile interrupted migration and retry");

        assert_eq!(
            loaded.conversion.archive_password.as_deref(),
            Some("legacy-after-crash")
        );
        assert!(crate::secret_store::get(&orphan).is_err());
        let rewritten = std::fs::read_to_string(&path).expect("read rewritten config");
        let persisted: TonepoetConfig = toml::from_str(&rewritten).expect("parse rewritten config");
        let published = persisted
            .conversion
            .archive_password_ref
            .as_deref()
            .expect("published replacement reference");
        assert_ne!(published, orphan.as_str());
        assert_eq!(
            crate::secret_store::get(published).expect("published secret"),
            "legacy-after-crash"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn config_load_preserves_reference_published_before_crash_and_retires_journal() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let reference = crate::secret_store::allocate_reference();
        crate::secret_store::begin_pending_publication(
            &path,
            std::slice::from_ref(&reference),
        )
        .expect("journal simulated publication");
        crate::secret_store::set(&reference, "published-before-crash")
            .expect("store secret");
        let mut persisted = TonepoetConfig::default();
        persisted.conversion.archive_password_ref = Some(reference.clone());
        std::fs::write(
            &path,
            toml::to_string_pretty(&persisted).expect("serialize referenced config"),
        )
        .expect("publish reference without retiring journal");

        let loaded = TonepoetConfig::load_from_path(&path)
            .expect("reconcile published reference");

        assert_eq!(loaded.conversion.archive_password, None);
        assert_eq!(
            loaded.conversion.archive_password_ref.as_deref(),
            Some(reference.as_str())
        );
        assert_eq!(
            crate::secret_store::get(&reference).expect("published secret retained"),
            "published-before-crash"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
        assert!(!crate::secret_store::pending_publication_path(&path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn config_load_migrates_cleartext_with_backup_and_runtime_rehydration() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let baseline = toml::to_string_pretty(&TonepoetConfig::default())
            .expect("serialize baseline config");
        let legacy = baseline.replacen(
            "[conversion]\n",
            "[conversion]\narchive_password = \"legacy-config-secret\"\n",
            1,
        );
        std::fs::write(&path, &legacy).expect("legacy config");
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set permissive legacy mode");

        let loaded = TonepoetConfig::load_from_path(&path).expect("migrate legacy config");

        assert_eq!(
            loaded.conversion.archive_password.as_deref(),
            Some("legacy-config-secret")
        );
        let rewritten = std::fs::read_to_string(&path).expect("rewritten config");
        assert!(!rewritten.contains("legacy-config-secret"));
        let persisted: TonepoetConfig = toml::from_str(&rewritten).expect("rewritten config parses");
        let reference = persisted
            .conversion
            .archive_password_ref
            .as_deref()
            .expect("migrated reference");
        assert_eq!(
            crate::secret_store::get(reference).expect("migrated secret"),
            "legacy-config-secret"
        );
        let migration_backup = temp.path().join("config.toml.pre-keychain-migration");
        assert_eq!(
            std::fs::read_to_string(&migration_backup).expect("migration backup"),
            legacy
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&migration_backup)
                .expect("migration backup metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn config_atomic_save_failure_removes_temp_and_preserves_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        std::fs::create_dir(&path).expect("directory target blocks atomic rename");

        let error = TonepoetConfig::default()
            .save_to_path(&path)
            .expect_err("renaming over a directory must fail");
        assert!(
            error.to_string().contains("Is a directory")
                || error.to_string().contains("directory")
                || error.to_string().contains("Access is denied"),
            "unexpected error: {error}"
        );
        assert!(path.is_dir(), "failed save must leave the existing target intact");
        let temp_leftovers = std::fs::read_dir(temp.path())
            .expect("list parent")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".config.toml.tmp"))
            .count();
        assert_eq!(temp_leftovers, 0, "failed atomic save must clean up its temp file");
    }

    #[test]
    fn config_save_replaces_existing_regular_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, b"old = true\n").expect("old config");
        let mut config = TonepoetConfig::default();
        config.conversion.write_log_file = true;

        let outcome = config
            .save_to_path_with_outcome(&path)
            .expect("replace existing regular file");

        assert_no_secret_publication_outcome(outcome);
        let encoded = std::fs::read_to_string(&path).expect("new config");
        assert!(encoded.contains("write_log_file = true"));
        assert!(!encoded.contains("old = true"));
    }

    #[cfg(unix)]
    #[test]
    fn config_save_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, b"old = true\n").expect("old config");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
            .expect("set mode");

        TonepoetConfig::default()
            .save_to_path(&path)
            .expect("save config");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[cfg(unix)]
    #[test]
    fn config_save_follows_final_symlink_instead_of_replacing_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real_dir = temp.path().join("dotfiles");
        std::fs::create_dir(&real_dir).expect("real dir");
        let real_path = real_dir.join("tonepoet.toml");
        std::fs::write(&real_path, b"old = true\n").expect("real config");
        let link_path = temp.path().join("config.toml");
        std::os::unix::fs::symlink(&real_path, &link_path).expect("symlink");
        let mut config = TonepoetConfig::default();
        config.conversion.write_log_file = true;

        config.save_to_path(&link_path).expect("save through symlink");

        assert!(std::fs::symlink_metadata(&link_path)
            .expect("link metadata")
            .file_type()
            .is_symlink());
        let encoded = std::fs::read_to_string(&real_path).expect("target updated");
        assert!(encoded.contains("write_log_file = true"));
        assert!(!encoded.contains("old = true"));
    }

    #[cfg(unix)]
    #[test]
    fn config_save_resolves_complete_final_symlink_chain() {
        let temp = tempfile::tempdir().expect("tempdir");
        let actual_dir = temp.path().join("actual");
        std::fs::create_dir(&actual_dir).expect("actual dir");
        let actual = actual_dir.join("tonepoet.toml");
        std::fs::write(&actual, b"old = true\n").expect("actual config");
        let middle = temp.path().join("current-config");
        let entry = temp.path().join("config.toml");
        std::os::unix::fs::symlink(&actual, &middle).expect("middle symlink");
        std::os::unix::fs::symlink("current-config", &entry).expect("entry symlink");
        let mut config = TonepoetConfig::default();
        config.conversion.write_log_file = true;

        config.save_to_path(&entry).expect("save through complete chain");

        assert!(std::fs::symlink_metadata(&entry)
            .expect("entry metadata")
            .file_type()
            .is_symlink());
        assert!(std::fs::symlink_metadata(&middle)
            .expect("middle metadata")
            .file_type()
            .is_symlink());
        let encoded = std::fs::read_to_string(&actual).expect("actual target updated");
        assert!(encoded.contains("write_log_file = true"));
        assert!(!encoded.contains("old = true"));
    }

    #[cfg(unix)]
    #[test]
    fn config_save_rejects_symlink_cycle_without_replacing_either_link() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("config.toml");
        let second = temp.path().join("current-config");
        std::os::unix::fs::symlink("current-config", &first).expect("first symlink");
        std::os::unix::fs::symlink("config.toml", &second).expect("second symlink");

        let error = TonepoetConfig::default()
            .save_to_path(&first)
            .expect_err("symlink cycle must fail closed")
            .to_string();

        assert_eq!(
            error,
            format!(
                "configuration path symlink cycle detected at '{}'",
                first.display()
            )
        );
        assert_eq!(
            std::fs::read_link(&first).expect("first remains"),
            PathBuf::from("current-config")
        );
        assert_eq!(
            std::fs::read_link(&second).expect("second remains"),
            PathBuf::from("config.toml")
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_save_rejects_excessive_symlink_depth_without_mutation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let entry = temp.path().join("config.toml");
        for index in 0..=40usize {
            let current = if index == 0 {
                entry.clone()
            } else {
                temp.path().join(format!("link-{index}"))
            };
            let next = if index == 40 {
                PathBuf::from("actual-config")
            } else {
                PathBuf::from(format!("link-{}", index + 1))
            };
            std::os::unix::fs::symlink(&next, &current).expect("build deep symlink chain");
        }
        let actual = temp.path().join("actual-config");
        std::fs::write(&actual, b"authoritative bytes").expect("actual target");

        let error = TonepoetConfig::default()
            .save_to_path(&entry)
            .expect_err("over-deep chain must fail closed")
            .to_string();

        assert_eq!(
            error,
            format!(
                "configuration path '{}' exceeds the maximum symlink depth of 40",
                entry.display()
            )
        );
        assert_eq!(
            std::fs::read(&actual).expect("actual remains unchanged"),
            b"authoritative bytes"
        );
        assert!(std::fs::symlink_metadata(&entry)
            .expect("entry remains")
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn config_save_removes_stale_temporary_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let stale = temp.path().join(".config.toml.tmp.crashed.123");
        let stale_backup = temp.path().join(".config.toml.replace-backup.crashed.123");
        std::fs::write(&stale, b"secret = true\n").expect("stale temp");
        std::fs::write(
            &stale_backup,
            toml::to_string_pretty(&TonepoetConfig::default())
                .expect("serialize valid stale backup"),
        )
        .expect("stale backup");

        let outcome = TonepoetConfig::default()
            .save_to_path_with_outcome(&path)
            .expect("replace config after stale-artifact recovery");
        assert_no_secret_publication_outcome(outcome);

        assert!(!stale.exists(), "save should recover stale temp files containing full config content");
        assert!(!stale_backup.exists(), "save should recover stale replace backups containing config content");
    }

    #[cfg(unix)]
    #[test]
    fn abandoned_config_save_lock_sidecar_does_not_block_or_delay_save() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let stale_tmp = temp.path().join(".config.toml.tmp.crashed.1");
        let lock = store_lock_path(temp.path(), &path);
        std::fs::write(&stale_tmp, b"archive_password = secret\n").expect("stale temp");
        let legacy_marker = format!("pid=4242 target={}\n", path.display());
        std::fs::write(&lock, legacy_marker.as_bytes())
            .expect("abandoned legacy lock marker");

        let outcome = TonepoetConfig::default()
            .save_to_path_with_outcome(&path)
            .expect("OS-backed lock must not treat an abandoned sidecar file as an active save");
        assert_no_secret_publication_outcome(outcome);

        assert!(path.exists(), "save should complete immediately after crash recovery");
        assert!(!stale_tmp.exists(), "stale temp containing serialized config should be removed after the lock is acquired");
        assert_eq!(
            std::fs::read(&lock).expect("read retained legacy marker"),
            legacy_marker.as_bytes(),
            "an abandoned valid marker remains non-authoritative and is not rewritten"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn active_config_save_lock_rejects_concurrent_saver_without_temp_cleanup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let active_tmp = temp.path().join(".config.toml.tmp.other.1");
        std::fs::write(&active_tmp, b"archive_password = secret\n").expect("active temp");
        let _lock = StoreFileLock::acquire(temp.path(), &path).expect("hold save lock");

        let error = TonepoetConfig::default()
            .save_to_path(&path)
            .expect_err("concurrent save should be rejected while OS lock is held");

        assert_eq!(
            error.to_string(),
            format!(
                "timed out after 2000 ms waiting for store update lock: {}",
                store_lock_path(temp.path(), &path).display()
            )
        );
        assert!(active_tmp.exists(), "must not clean active temp files while another saver owns the OS lock");
    }

    #[test]
    fn two_valid_replacement_backups_fail_closed_without_selecting_by_timestamp() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let first = temp.path().join(".config.toml.replace-backup.crashed.1");
        let second = temp.path().join(".config.toml.replace-backup.crashed.2");
        let mut first_config = TonepoetConfig::default();
        first_config.conversion.archive_password_ref = Some("tonepoet-secret:first".to_string());
        let mut second_config = TonepoetConfig::default();
        second_config.conversion.archive_password_ref = Some("tonepoet-secret:second".to_string());
        std::fs::write(
            &first,
            toml::to_string_pretty(&first_config).expect("serialize first backup"),
        )
        .expect("first backup");
        std::fs::write(
            &second,
            toml::to_string_pretty(&second_config).expect("serialize second backup"),
        )
        .expect("second backup");

        let error = TonepoetConfig::load_from_path(&path)
            .expect_err("multiple plausible backups must not be guessed")
            .to_string();

        assert_eq!(
            error,
            format!(
                "cannot recover missing configuration '{}': multiple valid replacement backups are present: '{}', '{}'",
                path.display(),
                first.display(),
                second.display()
            )
        );
        assert!(!path.exists());
        assert!(first.exists());
        assert!(second.exists());
    }

    #[test]
    fn malformed_replacement_backup_is_rejected_without_cleanup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let backup = temp.path().join(".config.toml.replace-backup.crashed.1");
        std::fs::write(&backup, b"not valid tonepoet config = [").expect("malformed backup");

        let error = TonepoetConfig::load_from_path(&path)
            .expect_err("malformed backup must fail closed")
            .to_string();

        assert_eq!(
            error,
            format!(
                "stale configuration replacement backup '{}' is not a valid Tonepoet configuration",
                backup.display()
            )
        );
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(&backup).expect("malformed backup retained"),
            b"not valid tonepoet config = ["
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_replacement_backup_is_rejected_without_following_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let target = temp.path().join("outside-config");
        let backup = temp.path().join(".config.toml.replace-backup.crashed.1");
        let bytes = toml::to_string_pretty(&TonepoetConfig::default())
            .expect("serialize target config");
        std::fs::write(&target, &bytes).expect("target config");
        std::os::unix::fs::symlink(&target, &backup).expect("symlink backup");

        let error = TonepoetConfig::load_from_path(&path)
            .expect_err("symlinked backup must fail closed")
            .to_string();

        assert_eq!(
            error,
            format!(
                "stale configuration replacement backup '{}' is not a regular file",
                backup.display()
            )
        );
        assert!(!path.exists());
        assert!(std::fs::symlink_metadata(&backup)
            .expect("backup remains")
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&target).expect("target retained"), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_sync_failure_retains_backup_journal_and_credential_authority() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let backup = temp.path().join(".config.toml.replace-backup.crashed.1");
        let reference = crate::secret_store::stable_reference("recovery-test", "sync-failure")
            .expect("stable reference");
        crate::secret_store::set(&reference, "retained-secret")
            .expect("store recovery secret");
        crate::secret_store::begin_pending_publication(
            &path,
            std::slice::from_ref(&reference),
        )
        .expect("write pending journal");
        let mut config = TonepoetConfig::default();
        config.conversion.archive_password_ref = Some(reference.clone());
        std::fs::write(
            &backup,
            toml::to_string_pretty(&config).expect("serialize backup"),
        )
        .expect("backup");
        let _injection = RecoveryArtifactInjectionGuard::fail_next_directory_sync(
            "synthetic recovery directory sync failure",
        );

        let error = TonepoetConfig::load_from_path(&path)
            .expect_err("nondurable recovery must stop before secret reconciliation")
            .to_string();

        assert_eq!(
            error,
            format!(
                "configuration recovery replaced '{}', but parent-directory durability could not be confirmed: synthetic recovery directory sync failure; replacement backup '{}' was retained and secret reconciliation was not attempted",
                path.display(),
                backup.display()
            )
        );
        assert!(path.exists(), "rename occurred but was not certified durable");
        assert!(backup.exists(), "authoritative backup must remain available");
        assert!(crate::secret_store::pending_publication_path(&path).exists());
        assert_eq!(
            crate::secret_store::get(&reference).expect("credential retained"),
            "retained-secret"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn stale_temporary_cleanup_failure_is_visible_and_retains_secret_authority() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let stale = temp.path().join(".config.toml.tmp.crashed.1");
        std::fs::write(
            &path,
            toml::to_string_pretty(&TonepoetConfig::default()).expect("serialize config"),
        )
        .expect("published config");
        std::fs::write(&stale, b"historical cleartext = true\n").expect("stale temp");
        let reference = crate::secret_store::stable_reference("recovery-test", "cleanup-failure")
            .expect("stable reference");
        crate::secret_store::set(&reference, "unreconciled-secret")
            .expect("store pending secret");
        crate::secret_store::begin_pending_publication(
            &path,
            std::slice::from_ref(&reference),
        )
        .expect("pending journal");
        let _injection = RecoveryArtifactInjectionGuard::fail_remove(
            stale.clone(),
            "synthetic stale temporary cleanup failure",
        );

        let error = TonepoetConfig::load_from_path(&path)
            .expect_err("cleanup failure must be visible")
            .to_string();

        assert_eq!(
            error,
            format!(
                "remove stale configuration artifact '{}': synthetic stale temporary cleanup failure",
                stale.display()
            )
        );
        assert_eq!(
            std::fs::read(&stale).expect("stale temp retained"),
            b"historical cleartext = true\n"
        );
        assert!(crate::secret_store::pending_publication_path(&path).exists());
        assert_eq!(
            crate::secret_store::get(&reference).expect("credential retained"),
            "unreconciled-secret"
        );
        assert_eq!(crate::secret_store::insecure_test_secret_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn single_valid_backup_is_restored_durably_before_cleanup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let backup = temp.path().join(".config.toml.replace-backup.crashed.1");
        let bytes = toml::to_string_pretty(&TonepoetConfig::default())
            .expect("serialize valid backup");
        std::fs::write(&backup, &bytes).expect("backup");

        recover_stale_config_artifacts(temp.path(), &path).expect("recover artifacts");

        assert_eq!(std::fs::read_to_string(&path).expect("restored"), bytes);
        assert!(!backup.exists(), "backup should be removed only after durable restore");
    }

    #[test]
    fn config_save_reports_post_rename_durability_failure_without_rollback_claim() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let tmp = temp.path().join(".config.toml.tmp.test");

        let outcome = write_and_publish_config_temp_with_sync(
            &path,
            &tmp,
            b"new = true\n",
            0o600,
            |_parent| Err(std::io::Error::new(std::io::ErrorKind::Other, "sync failed")),
        )
        .expect("rename succeeds; sync warning is an outcome");

        assert_eq!(
            outcome,
            ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(
                "config.toml was written, but parent-directory durability could not be confirmed: sync failed".to_string()
            )
        );
        assert_eq!(std::fs::read_to_string(&path).expect("published"), "new = true\n");
        assert!(!tmp.exists(), "published temp path should be gone after rename");
    }

    #[test]
    fn public_save_returns_exact_error_when_publication_durability_is_unconfirmed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let _injection = PublicSaveInjectionGuard::install(
            path.clone(),
            "synthetic public-save sync failure",
        );

        let error = TonepoetConfig::default()
            .save()
            .expect_err("the production save API must not discard durability failure")
            .to_string();

        assert_eq!(
            error,
            "config.toml was written, but parent-directory durability could not be confirmed: synthetic public-save sync failure; no secret-store mutation was pending"
        );
        let persisted: TonepoetConfig = toml::from_str(
            &std::fs::read_to_string(&path).expect("replacement was published"),
        )
        .expect("published config parses");
        assert_eq!(persisted.conversion.archive_password, None);
        assert_eq!(persisted.conversion.archive_password_ref, None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_config_replacement_is_never_classified_as_durable_without_write_through() {
        let temp = tempfile::tempdir().expect("tempdir");
        let structured_path = temp.path().join("structured.toml");
        let strict_path = temp.path().join("strict.toml");
        let expected =
            "config.toml was written, but parent-directory durability could not be confirmed: Windows replacement was not performed with write-through semantics; no secret-store mutation was pending";

        let outcome = TonepoetConfig::default()
            .save_to_path_with_outcome(&structured_path)
            .expect("replacement itself succeeds");
        assert_eq!(
            outcome,
            ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(expected.to_string())
        );
        assert!(structured_path.exists());

        let error = TonepoetConfig::default()
            .save_to_path(&strict_path)
            .expect_err("strict save must reject unconfirmed Windows durability")
            .to_string();
        assert_eq!(error, expected);
        assert!(strict_path.exists());
    }

}
