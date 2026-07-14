use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TonepoetConfig {
    pub conversion: ConversionSettings,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
    #[serde(default)]
    pub browsing: BrowsingConfig,
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
            convert_default_action: default_convert_action(),
            compare_keep_reference: false,
            theme: crate::tui::theme::default_theme_name(),
        }
    }
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
    /// Default archive password
    pub archive_password: Option<String>,
    /// Default ordered pre/post conversion action pipeline.
    #[serde(default, skip_serializing_if = "crate::convert::pipeline::ActionPipeline::is_empty")]
    pub actions: crate::convert::pipeline::ActionPipeline,
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
            actions: crate::convert::pipeline::ActionPipeline::default(),
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
        }
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSaveOutcome {
    Durable,
    ReplacedButDurabilityUnconfirmed(String),
}

impl ConfigSaveOutcome {
    pub fn durability_warning(&self) -> Option<&str> {
        match self {
            Self::Durable => None,
            Self::ReplacedButDurabilityUnconfirmed(message) => Some(message.as_str()),
        }
    }
}

struct ConfigSaveLock {
    _path: PathBuf,
    _file: fs::File,
}

impl ConfigSaveLock {
    fn acquire(parent: &Path, target_path: &Path) -> anyhow::Result<Self> {
        let lock_path = config_sidecar_path(parent, target_path, "save.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(&lock_path)?;
        lock_config_file(&file).map_err(|error| {
            let lock_is_held = error.kind() == std::io::ErrorKind::WouldBlock
                || error.raw_os_error() == Some(33); // Windows ERROR_LOCK_VIOLATION
            if lock_is_held {
                anyhow::anyhow!("config save already in progress: {}", lock_path.display())
            } else {
                anyhow::Error::from(error)
            }
        })?;

        // Advisory locks are owned by the open file description/handle and are
        // released by the OS if the process exits. The sidecar file is only
        // diagnostic; it is intentionally not deleted on Drop, so a resumed or
        // dying stale owner cannot remove a newer owner's lock pathname.
        let _ = file.set_len(0);
        let _ = writeln!(
            file,
            "pid={} target={}",
            std::process::id(),
            target_path.display()
        );
        let _ = file.sync_all();

        Ok(Self { _path: lock_path, _file: file })
    }
}

#[cfg(unix)]
fn lock_config_file(file: &fs::File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;

    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }

    let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn lock_config_file(file: &fs::File) -> std::io::Result<()> {
    use std::ffi::c_void;
    use std::mem::zeroed;
    use std::os::windows::io::AsRawHandle;

    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x00000001;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x00000002;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }

    unsafe extern "system" {
        fn LockFileEx(
            hFile: *mut c_void,
            dwFlags: u32,
            dwReserved: u32,
            nNumberOfBytesToLockLow: u32,
            nNumberOfBytesToLockHigh: u32,
            lpOverlapped: *mut Overlapped,
        ) -> i32;
    }

    let mut overlapped: Overlapped = unsafe { zeroed() };
    let rc = unsafe {
        LockFileEx(
            file.as_raw_handle() as *mut c_void,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if rc != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(unix, windows)))]
fn lock_config_file(_file: &fs::File) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "crash-safe config save locking is unsupported on this platform",
    ))
}

fn atomic_write_config(config_path: &Path, content: &[u8]) -> anyhow::Result<ConfigSaveOutcome> {
    let target_path = resolve_config_save_target(config_path)?;
    let parent = target_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent: {}", target_path.display()))?;
    fs::create_dir_all(parent)?;
    let _lock = ConfigSaveLock::acquire(parent, &target_path)?;
    recover_stale_config_artifacts(parent, &target_path)?;

    let file_name = target_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mode = config_file_mode(&target_path)?;

    let mut last_create_error = None;
    for attempt in 0..128u32 {
        let tmp_path = parent.join(format!(
            ".{file_name}.tmp.{}.{}.{}",
            std::process::id(),
            stamp,
            attempt
        ));
        match write_and_publish_config_temp(&target_path, &tmp_path, content, mode) {
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

fn resolve_config_save_target(config_path: &Path) -> anyhow::Result<PathBuf> {
    match fs::symlink_metadata(config_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let link = fs::read_link(config_path)?;
            Ok(if link.is_absolute() {
                link
            } else {
                config_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(link)
            })
        }
        Ok(_) | Err(_) => Ok(config_path.to_path_buf()),
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

fn config_sidecar_path(parent: &Path, target_path: &Path, suffix: &str) -> PathBuf {
    let file_name = target_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    parent.join(format!(".{file_name}.{suffix}"))
}

fn recover_stale_config_artifacts(parent: &Path, target_path: &Path) -> std::io::Result<()> {
    let Some(file_name) = target_path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let temp_prefix = format!(".{file_name}.tmp.");
    let backup_prefix = format!(".{file_name}.replace-backup.");
    let mut backups = Vec::new();
    let Ok(entries) = fs::read_dir(parent) else {
        return Ok(());
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&temp_prefix) {
            let _ = fs::remove_file(&path);
        } else if name.starts_with(&backup_prefix) {
            backups.push(path);
        }
    }

    if !target_path.exists() {
        backups.sort_by_key(|path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
        });
        if let Some(backup) = backups.pop() {
            fs::rename(&backup, target_path)?;
        }
    }

    for backup in backups {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn write_and_publish_config_temp(
    config_path: &Path,
    tmp_path: &Path,
    content: &[u8],
    mode: u32,
) -> std::io::Result<ConfigSaveOutcome> {
    write_and_publish_config_temp_with_sync(config_path, tmp_path, content, mode, sync_parent_dir)
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
        match sync_parent(config_path.parent().expect("validated parent")) {
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

#[cfg(unix)]
fn replace_config_file(tmp_path: &Path, config_path: &Path) -> std::io::Result<()> {
    fs::rename(tmp_path, config_path)
}

#[cfg(windows)]
fn replace_config_file(tmp_path: &Path, config_path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x00000001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x00000008;

    unsafe extern "system" {
        fn MoveFileExW(
            lpExistingFileName: *const u16,
            lpNewFileName: *const u16,
            dwFlags: u32,
        ) -> i32;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    let from = wide(tmp_path);
    let to = wide(config_path);
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_config_file(tmp_path: &Path, config_path: &Path) -> std::io::Result<()> {
    match fs::rename(tmp_path, config_path) {
        Ok(()) => Ok(()),
        Err(error) if config_path.exists() => Err(std::io::Error::new(
            error.kind(),
            format!(
                "atomic replacement of an existing config is unsupported on this platform; existing config left unchanged: {error}"
            ),
        )),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn sync_parent_dir(parent: &Path) -> std::io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_dir(_parent: &Path) -> std::io::Result<()> {
    Ok(())
}

impl TonepoetConfig {
    /// Load config from the default path (~/.config/tonepoet/config.toml)
    pub fn load() -> anyhow::Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let config: TonepoetConfig = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    /// Save config to the default path.
    ///
    /// The write is serialized with an OS-backed save lock, uses a
    /// same-directory temporary file, fsyncs it, atomically publishes it on
    /// supported platforms, preserves an existing file's permissions, follows a
    /// final symlink to support dotfile-managed configs, and fsyncs the parent
    /// directory when the platform exposes that durability primitive.
    pub fn save(&self) -> anyhow::Result<()> {
        self.save_with_outcome().map(|_| ())
    }

    pub fn save_with_outcome(&self) -> anyhow::Result<ConfigSaveOutcome> {
        self.save_to_path_with_outcome(Self::config_path())
    }

    /// Save config to an explicit path. This exists so UI persistence paths can
    /// be tested against temporary config files without mutating the user's
    /// real configuration.
    pub fn save_to_path<P: AsRef<Path>>(&self, config_path: P) -> anyhow::Result<()> {
        self.save_to_path_with_outcome(config_path).map(|_| ())
    }

    pub fn save_to_path_with_outcome<P: AsRef<Path>>(
        &self,
        config_path: P,
    ) -> anyhow::Result<ConfigSaveOutcome> {
        let config_path = config_path.as_ref();
        let target_path = resolve_config_save_target(config_path)?;
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        atomic_write_config(config_path, content.as_bytes())
    }

    /// Get the config file path
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tonepoet")
            .join("config.toml")
    }
}

#[cfg(test)]
mod theme_config_tests {
    use super::*;

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
"#,
        )
        .expect("config parses without performance");

        assert_eq!(config.performance.browsing.archive_listing, "auto");
        assert_eq!(config.performance.browsing.archive_listing_timeout, 30);
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
        config.save_to_path(&path).expect("atomic save");

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

        assert_eq!(outcome, ConfigSaveOutcome::Durable);
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

    #[test]
    fn config_save_removes_stale_temporary_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let stale = temp.path().join(".config.toml.tmp.crashed.123");
        let stale_backup = temp.path().join(".config.toml.replace-backup.crashed.123");
        std::fs::write(&stale, b"secret = true\n").expect("stale temp");
        std::fs::write(&stale_backup, b"old_secret = true\n").expect("stale backup");

        TonepoetConfig::default()
            .save_to_path(&path)
            .expect("save config");

        assert!(!stale.exists(), "save should recover stale temp files containing full config content");
        assert!(!stale_backup.exists(), "save should recover stale replace backups containing config content");
    }

    #[test]
    fn abandoned_config_save_lock_sidecar_does_not_block_or_delay_save() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let stale_tmp = temp.path().join(".config.toml.tmp.crashed.1");
        let lock = temp.path().join(".config.toml.save.lock");
        std::fs::write(&stale_tmp, b"archive_password = secret\n").expect("stale temp");
        std::fs::write(&lock, b"pid=crashed").expect("abandoned diagnostic lock file");

        TonepoetConfig::default()
            .save_to_path(&path)
            .expect("OS-backed lock must not treat an abandoned sidecar file as an active save");

        assert!(path.exists(), "save should complete immediately after crash recovery");
        assert!(!stale_tmp.exists(), "stale temp containing serialized config should be removed after the lock is acquired");
        assert!(lock.exists(), "the diagnostic lock sidecar may persist without blocking future saves");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn active_config_save_lock_rejects_concurrent_saver_without_temp_cleanup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let active_tmp = temp.path().join(".config.toml.tmp.other.1");
        std::fs::write(&active_tmp, b"archive_password = secret\n").expect("active temp");
        let _lock = ConfigSaveLock::acquire(temp.path(), &path).expect("hold save lock");

        let error = TonepoetConfig::default()
            .save_to_path(&path)
            .expect_err("concurrent save should be rejected while OS lock is held");

        assert!(error.to_string().contains("already in progress"));
        assert!(active_tmp.exists(), "must not clean active temp files while another saver owns the OS lock");
    }

    #[test]
    fn stale_non_unix_backup_is_restored_before_cleanup_when_target_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        let backup = temp.path().join(".config.toml.replace-backup.crashed.1");
        std::fs::write(&backup, b"old = true\n").expect("backup");

        recover_stale_config_artifacts(temp.path(), &path).expect("recover artifacts");

        assert_eq!(std::fs::read_to_string(&path).expect("restored"), "old = true\n");
        assert!(!backup.exists(), "backup should have been moved back into place");
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

        assert!(matches!(outcome, ConfigSaveOutcome::ReplacedButDurabilityUnconfirmed(_)));
        assert_eq!(std::fs::read_to_string(&path).expect("published"), "new = true\n");
        assert!(!tmp.exists(), "published temp path should be gone after rename");
    }

}
