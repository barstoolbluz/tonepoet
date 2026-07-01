//! Parse `7zz l -slt` output into structured archive entries.
//!
//! The `-slt` (show technical listing) flag produces key-value pairs for
//! each entry, separated by lines of dashes. This parser extracts the
//! fields needed for browse-like navigation inside archives.

use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::path::Component;
use std::time::{Duration, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

/// A single entry inside an archive.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// Relative path inside the archive (uses `/` separators).
    pub path: String,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// Compressed size in bytes.
    pub packed_size: u64,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Whether this entry is encrypted.
    pub encrypted: bool,
}

impl ArchiveEntry {
    /// The filename (last component of the path).
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The parent directory path inside the archive, or empty string for root.
    pub fn parent_path(&self) -> &str {
        match self.path.rfind('/') {
            Some(pos) => &self.path[..pos],
            None => "",
        }
    }

    /// File extension (lowercase), if any.
    pub fn extension(&self) -> Option<String> {
        let name = self.file_name();
        name.rfind('.').map(|pos| name[pos + 1..].to_lowercase())
    }
}

/// Result of listing an archive.
#[derive(Debug, Clone)]
pub struct ArchiveListing {
    /// Archive file path on disk.
    pub archive_path: PathBuf,
    /// Archive format as reported by 7zz (e.g., "7z", "zip", "rar").
    pub format: String,
    /// Total physical size on disk.
    pub physical_size: u64,
    /// All entries (files and directories).
    pub entries: Vec<ArchiveEntry>,
}

impl ArchiveListing {
    /// Approximate heap footprint of this listing when retained in the in-memory
    /// Browse cache. The value is intentionally conservative enough for eviction
    /// decisions without depending on allocator internals.
    pub fn estimated_cache_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.archive_path.as_os_str().to_string_lossy().len()
            + self.format.len()
            + self.entries.capacity() * std::mem::size_of::<ArchiveEntry>()
            + self.entries.iter().map(|entry| entry.path.len()).sum::<usize>()
    }

    /// List entries at a specific directory level inside the archive.
    /// Returns entries whose parent_path matches `dir` (empty string = root).
    /// Deduplicates implicit directories (archives often don't list dirs explicitly).
    pub fn entries_at(&self, dir: &str) -> Vec<ArchiveListItem> {
        use std::collections::BTreeSet;

        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{}/", dir)
        };

        let mut dirs_seen: BTreeSet<String> = BTreeSet::new();
        let mut files_seen: BTreeSet<String> = BTreeSet::new();
        let mut items = Vec::new();

        for entry in &self.entries {
            // Entry must be under the given directory.
            let relative = if prefix.is_empty() {
                entry.path.as_str()
            } else if let Some(rest) = entry.path.strip_prefix(&prefix) {
                rest
            } else {
                continue;
            };

            // Skip empty paths (the directory itself).
            if relative.is_empty() {
                continue;
            }

            if let Some(slash_pos) = relative.find('/') {
                // Entry is in a subdirectory - register the immediate child dir.
                let child_dir = &relative[..slash_pos];
                let full_dir = if prefix.is_empty() {
                    child_dir.to_string()
                } else {
                    format!("{}{}", prefix, child_dir)
                };
                if dirs_seen.insert(full_dir.clone()) {
                    items.push(ArchiveListItem {
                        name: child_dir.to_string(),
                        full_path: full_dir,
                        is_dir: true,
                        size: 0,
                        packed_size: 0,
                    });
                }
            } else if entry.is_dir {
                if dirs_seen.insert(entry.path.clone()) {
                    items.push(ArchiveListItem {
                        name: relative.to_string(),
                        full_path: entry.path.clone(),
                        is_dir: true,
                        size: 0,
                        packed_size: 0,
                    });
                }
            } else if files_seen.insert(entry.path.clone()) {
                // Direct child file.
                items.push(ArchiveListItem {
                    name: relative.to_string(),
                    full_path: entry.path.clone(),
                    is_dir: false,
                    size: entry.size,
                    packed_size: entry.packed_size,
                });
            }
        }

        // Sort: dirs first (alphabetical), then files (alphabetical).
        items.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });

        items
    }
}

/// A display-ready item at a specific directory level.
#[derive(Debug, Clone)]
pub struct ArchiveListItem {
    /// Display name (just the filename or directory name, no path).
    pub name: String,
    /// Full path inside the archive.
    pub full_path: String,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// Uncompressed size (0 for directories).
    pub size: u64,
    /// Compressed size (0 for directories).
    pub packed_size: u64,
}

/// Stable in-memory cache key for an archive listing.
///
/// The path identifies the archive; size and mtime invalidate stale listings
/// when the file changes in place.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArchiveListingCacheKey {
    pub path: PathBuf,
    pub size: u64,
    pub modified_secs: u64,
    pub modified_nanos: u32,
}

impl ArchiveListingCacheKey {
    pub fn for_path(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        let duration = modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0));
        Ok(Self {
            path: canonical_cache_path(path),
            size: metadata.len(),
            modified_secs: duration.as_secs(),
            modified_nanos: duration.subsec_nanos(),
        })
    }
}

fn canonical_cache_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveListingMode {
    Auto,
    Always,
    Never,
}

impl ArchiveListingMode {
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "always" => Self::Always,
            "never" => Self::Never,
            _ => Self::Auto,
        }
    }

    pub fn config_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (skip remote)",
            Self::Always => "Always",
            Self::Never => "Never",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Always,
            Self::Always => Self::Never,
            Self::Never => Self::Auto,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Auto => Self::Never,
            Self::Always => Self::Auto,
            Self::Never => Self::Always,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ArchiveListingOptions {
    /// Timeout for the 7z listing process. `None` means no timeout.
    pub timeout: Option<Duration>,
}

impl Default for ArchiveListingOptions {
    fn default() -> Self {
        Self {
            timeout: Some(Duration::from_secs(30)),
        }
    }
}

/// List an archive's contents using `7zz l -slt`. Returns the parsed
/// listing or an error string.
#[allow(dead_code)]
pub async fn list_archive(
    archive: &Path,
    password: Option<&str>,
) -> Result<ArchiveListing, String> {
    list_archive_with_options(
        archive,
        password,
        ArchiveListingOptions::default(),
        CancellationToken::new(),
    )
    .await
}

/// Cancellable, timeout-bounded variant of [`list_archive`].
///
/// The child process never inherits terminal stdin. This is critical while the
/// TUI has the terminal in raw mode: password prompts and other interactive 7z
/// questions must receive EOF rather than stealing or waiting for user input.
pub async fn list_archive_with_options(
    archive: &Path,
    password: Option<&str>,
    options: ArchiveListingOptions,
    cancel: CancellationToken,
) -> Result<ArchiveListing, String> {
    use tokio::process::Command;

    if cancel.is_cancelled() {
        return Err("archive listing cancelled".to_string());
    }

    let bin =
        crate::detect_7z_binary().ok_or_else(|| "neither 7zz nor 7z found in PATH".to_string())?;

    let mut cmd = Command::new(bin);
    cmd.arg("l").arg("-slt").arg(archive);
    if let Some(pw) = password {
        cmd.arg(format!("-p{}", pw));
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let output_future = cmd.output();
    let output = match options.timeout {
        Some(timeout) if !timeout.is_zero() => {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err("archive listing cancelled".to_string());
                }
                result = tokio::time::timeout(timeout, output_future) => {
                    match result {
                        Ok(output) => output,
                        Err(_) => return Err(format!("archive listing timed out after {}s", timeout.as_secs())),
                    }
                }
            }
        }
        _ => {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err("archive listing cancelled".to_string());
                }
                output = output_future => output,
            }
        }
    }
    .map_err(|e| format!("failed to run {}: {}", bin, e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}\n{}", stdout, stderr);

    if !output.status.success() {
        if looks_like_missing_password(&combined) {
            return Err("archive requires a password; set one with :password or in Config > Archive Passwords".into());
        }
        if looks_like_wrong_password(&combined) {
            return Err("wrong archive password".into());
        }
        if combined.trim().is_empty() {
            return Err(format!("{} listing failed with exit status {}", bin, output.status));
        }
        return Err(format!("{} listing failed: {}", bin, collapse_error_text(&combined)));
    }

    parse_slt_output(&stdout, archive)
}

fn looks_like_missing_password(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    (lower.contains("enter password") || lower.contains("password is required"))
        && !lower.contains("wrong password")
}

fn looks_like_wrong_password(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("wrong password")
        || lower.contains("can not open encrypted archive")
        || lower.contains("cannot open encrypted archive")
        || (lower.contains("can not open the file as archive") && lower.contains("encrypted"))
}

fn collapse_error_text(text: &str) -> String {
    let collapsed = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if collapsed.len() > 500 {
        let mut end = 500;
        while !collapsed.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &collapsed[..end])
    } else {
        collapsed
    }
}

/// Return true if automatic archive listing should avoid this path because it
/// lives on a network/remote filesystem.
pub fn is_remote_filesystem(path: &Path) -> bool {
    is_remote_filesystem_impl(path).unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn is_remote_filesystem_impl(path: &Path) -> std::io::Result<bool> {
    let mounts = std::fs::read_to_string("/proc/mounts")?;
    is_remote_filesystem_from_mounts(path, &mounts)
}

#[cfg(not(target_os = "linux"))]
fn is_remote_filesystem_impl(_path: &Path) -> std::io::Result<bool> {
    Ok(false)
}

#[cfg(target_os = "linux")]
fn is_remote_filesystem_from_mounts(path: &Path, mounts: &str) -> std::io::Result<bool> {
    // This path runs synchronously on Browse navigation before we know whether
    // the user actually wants to list the archive. Keep it metadata-free: no
    // stat, no canonicalize, no symlink resolution. We only need a conservative
    // mount-point match to decide whether auto-listing should opt out.
    let target = lexical_absolute_path(path)?;
    let mut best_len = 0usize;
    let mut best_remote = false;

    for line in mounts.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let mount_point = normalize_lexical_path(Path::new(&unescape_proc_mount_field(fields[1])));
        if !path_is_under_mount(&target, &mount_point) {
            continue;
        }
        let len = mount_point.as_os_str().to_string_lossy().len();
        if len >= best_len {
            best_len = len;
            best_remote = is_remote_filesystem_type(fields[2]);
        }
    }

    Ok(best_remote)
}

#[cfg(target_os = "linux")]
fn lexical_absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(normalize_lexical_path(&absolute))
}

#[cfg(target_os = "linux")]
fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                let at_root = normalized.has_root() && normalized.parent().is_none();
                if !at_root && !normalized.pop() {
                    normalized.push("..");
                }
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

#[cfg(target_os = "linux")]
fn unescape_proc_mount_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(target_os = "linux")]
fn path_is_under_mount(path: &Path, mount_point: &Path) -> bool {
    path == mount_point || path.starts_with(mount_point)
}

#[cfg(target_os = "linux")]
fn is_remote_filesystem_type(fs_type: &str) -> bool {
    let fs = fs_type.to_ascii_lowercase();
    fs == "nfs"
        || fs == "nfs4"
        || fs == "cifs"
        || fs == "smb3"
        || fs == "sshfs"
        || fs == "9p"
        || fs == "davfs"
        || fs == "glusterfs"
        || fs == "ceph"
        || fs == "lustre"
        || fs == "fuse"
        // Conservative by design: arbitrary FUSE mounts can be network-backed
        // (s3fs, gcsfuse, rclone, sshfs, curlftpfs, GVFS, and similar). Auto
        // mode should not probe them unless the user explicitly opts in.
        || fs.starts_with("fuse.")
}

/// Parse the `-slt` output into an `ArchiveListing`.
fn parse_slt_output(output: &str, archive: &Path) -> Result<ArchiveListing, String> {
    let mut format = String::new();
    let mut physical_size: u64 = 0;
    let mut entries = Vec::new();

    // The output has a header section (archive metadata) followed by
    // entry sections separated by lines of dashes.
    let mut in_entries = false;
    let mut current: Option<EntryBuilder> = None;

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("----------") {
            // Commit the previous entry (if any).
            if let Some(builder) = current.take() {
                if let Some(entry) = builder.build() {
                    entries.push(entry);
                }
            }
            in_entries = true;
            current = Some(EntryBuilder::default());
            continue;
        }

        if let Some((key, value)) = parse_kv(trimmed) {
            if in_entries {
                // 7zz v25+ doesn't always emit "----------" between entries.
                // A new "Path = ..." while we already have a path signals
                // the start of the next entry.
                if key == "Path" {
                    if let Some(builder) = current.take() {
                        if builder.path.is_some() {
                            if let Some(entry) = builder.build() {
                                entries.push(entry);
                            }
                        }
                    }
                    let mut new_builder = EntryBuilder::default();
                    new_builder.set(key, value);
                    current = Some(new_builder);
                    continue;
                }
                if let Some(ref mut builder) = current {
                    builder.set(key, value);
                }
            } else {
                // Header section.
                match key {
                    "Type" => format = value.to_string(),
                    "Physical Size" => {
                        physical_size = value.parse().unwrap_or(0);
                    }
                    _ => {}
                }
            }
        }
    }

    // Commit the last entry.
    if let Some(builder) = current {
        if let Some(entry) = builder.build() {
            entries.push(entry);
        }
    }

    Ok(ArchiveListing {
        archive_path: archive.to_path_buf(),
        format,
        physical_size,
        entries,
    })
}

/// Parse a `Key = Value` line.
fn parse_kv(line: &str) -> Option<(&str, &str)> {
    let eq_pos = line.find(" = ")?;
    let key = line[..eq_pos].trim();
    let value = line[eq_pos + 3..].trim();
    Some((key, value))
}

/// Builder for accumulating entry fields from key-value pairs.
#[derive(Default)]
struct EntryBuilder {
    path: Option<String>,
    size: u64,
    packed_size: u64,
    is_dir: bool,
    encrypted: bool,
}

impl EntryBuilder {
    fn set(&mut self, key: &str, value: &str) {
        match key {
            "Path" => self.path = Some(value.to_string()),
            "Size" => self.size = value.parse().unwrap_or(0),
            "Packed Size" => self.packed_size = value.parse().unwrap_or(0),
            "Folder" => self.is_dir = value == "+",
            "Attributes" => {
                // Attributes like "D" or "D...." indicate directory.
                if value.starts_with('D') {
                    self.is_dir = true;
                }
            }
            "Encrypted" => self.encrypted = value == "+",
            _ => {}
        }
    }

    fn build(self) -> Option<ArchiveEntry> {
        let path = self.path?;
        // Skip empty paths.
        if path.is_empty() {
            return None;
        }
        // Normalise Windows backslashes to forward slashes.
        let path = path.replace('\\', "/");
        // Strip trailing slash from directories.
        let path = path.trim_end_matches('/').to_string();
        if path.is_empty() {
            return None;
        }
        Some(ArchiveEntry {
            path,
            size: self.size,
            packed_size: self.packed_size,
            is_dir: self.is_dir,
            encrypted: self.encrypted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OUTPUT: &str = r#"
7-Zip (z) 25.01 (x64) : Copyright (c) 1999-2025 Igor Pavlov : 2025-08-03

Listing archive: /tmp/test.7z

--
Path = /tmp/test.7z
Type = 7z
Physical Size = 1234
Headers Size = 100
Method = LZMA2:12

----------
Path = Music
Size = 0
Packed Size = 0
Folder = +
Attributes = D drwxr-xr-x
Encrypted = -

----------
Path = Music/01 - Song.flac
Size = 30000000
Packed Size = 25000000
Modified = 2025-01-15 10:30:00
Attributes = A -rw-r--r--
CRC = AABBCCDD
Encrypted = -
Method = LZMA2:12

----------
Path = Music/02 - Track.flac
Size = 28000000
Packed Size = 23000000
Modified = 2025-01-15 10:31:00
Attributes = A -rw-r--r--
CRC = 11223344
Encrypted = -
Method = LZMA2:12

----------
Path = cover.jpg
Size = 500000
Packed Size = 490000
Modified = 2025-01-15 10:32:00
Attributes = A -rw-r--r--
Encrypted = -
"#;

    #[test]
    fn parse_listing() {
        let listing = parse_slt_output(SAMPLE_OUTPUT, Path::new("/tmp/test.7z")).unwrap();
        assert_eq!(listing.format, "7z");
        assert_eq!(listing.physical_size, 1234);
        assert_eq!(listing.entries.len(), 4); // Music dir + 2 flacs + cover.jpg

        let music = &listing.entries[0];
        assert_eq!(music.path, "Music");
        assert!(music.is_dir);

        let song = &listing.entries[1];
        assert_eq!(song.path, "Music/01 - Song.flac");
        assert_eq!(song.size, 30000000);
        assert!(!song.is_dir);
        assert_eq!(song.file_name(), "01 - Song.flac");
        assert_eq!(song.parent_path(), "Music");
        assert_eq!(song.extension(), Some("flac".into()));
    }

    #[test]
    fn entries_at_root() {
        let listing = parse_slt_output(SAMPLE_OUTPUT, Path::new("/tmp/test.7z")).unwrap();
        let root = listing.entries_at("");
        // Root should have: Music/ dir + cover.jpg
        assert_eq!(root.len(), 2);
        assert!(root[0].is_dir);
        assert_eq!(root[0].name, "Music");
        assert!(!root[1].is_dir);
        assert_eq!(root[1].name, "cover.jpg");
    }

    #[test]
    fn entries_at_subdir() {
        let listing = parse_slt_output(SAMPLE_OUTPUT, Path::new("/tmp/test.7z")).unwrap();
        let music = listing.entries_at("Music");
        assert_eq!(music.len(), 2);
        assert_eq!(music[0].name, "01 - Song.flac");
        assert_eq!(music[1].name, "02 - Track.flac");
    }

    #[test]
    fn explicit_directory_only_archive_renders_directory_at_root() {
        let output = "----------\nPath = EmptyDir\nFolder = +\nAttributes = D\n";
        let listing = parse_slt_output(output, Path::new("test.zip")).unwrap();
        let root = listing.entries_at("");
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].name, "EmptyDir");
        assert!(root[0].is_dir);
    }

    #[test]
    fn backslash_normalisation() {
        let output = "----------\nPath = Dir\\Sub\\file.txt\nSize = 100\n";
        let listing = parse_slt_output(output, Path::new("test.zip")).unwrap();
        assert_eq!(listing.entries[0].path, "Dir/Sub/file.txt");
    }

    #[test]
    fn encrypted_entry() {
        let output = "----------\nPath = secret.flac\nSize = 100\nEncrypted = +\n";
        let listing = parse_slt_output(output, Path::new("test.7z")).unwrap();
        assert!(listing.entries[0].encrypted);
    }

    /// 7zz v25+ doesn't emit "----------" between entries - only before the
    /// first one. Entries are delimited by the next "Path = ..." line.
    #[test]
    fn no_separator_between_entries() {
        let output = r#"
--
Path = /tmp/test.7z
Type = 7z
Physical Size = 1000

----------
Path = Album
Size = 0
Packed Size = 0
Attributes = D
Encrypted = -

Path = Album/01 - Song.flac
Size = 30000000
Packed Size = 25000000
Attributes = A
Encrypted = +

Path = Album/02 - Track.flac
Size = 28000000
Packed Size = 23000000
Attributes = A
Encrypted = +

Path = cover.jpg
Size = 500000
Packed Size = 490000
Attributes = A
Encrypted = -
"#;
        let listing = parse_slt_output(output, Path::new("/tmp/test.7z")).unwrap();
        assert_eq!(listing.entries.len(), 4);
        assert_eq!(listing.entries[0].path, "Album");
        assert!(listing.entries[0].is_dir);
        assert_eq!(listing.entries[1].path, "Album/01 - Song.flac");
        assert!(!listing.entries[1].is_dir);
        assert!(listing.entries[1].encrypted);
        assert_eq!(listing.entries[2].path, "Album/02 - Track.flac");
        assert_eq!(listing.entries[3].path, "cover.jpg");

        // entries_at should work for subdirectory
        let album = listing.entries_at("Album");
        assert_eq!(album.len(), 2);
        assert_eq!(album[0].name, "01 - Song.flac");
        assert_eq!(album[1].name, "02 - Track.flac");
    }

    #[test]
    fn password_errors_are_classified() {
        assert!(looks_like_missing_password("Enter password (will not be echoed):"));
        assert!(looks_like_wrong_password("ERROR: Wrong password"));
    }

    #[test]
    fn listing_mode_cycles_and_sanitizes_config() {
        assert_eq!(ArchiveListingMode::from_config("always"), ArchiveListingMode::Always);
        assert_eq!(ArchiveListingMode::from_config("never"), ArchiveListingMode::Never);
        assert_eq!(ArchiveListingMode::from_config("bogus"), ArchiveListingMode::Auto);
        assert_eq!(ArchiveListingMode::Auto.next(), ArchiveListingMode::Always);
        assert_eq!(ArchiveListingMode::Auto.previous(), ArchiveListingMode::Never);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_mount_detection_normalizes_lexically_without_requiring_existing_paths() {
        let cwd = std::env::current_dir().unwrap();
        let archive = cwd.join("does-not-exist/../remote/a.zip");
        let mount = cwd.join("remote");
        let mount_field = mount.to_string_lossy().replace(' ', "\\040");
        let mounts = format!("remote {} fuse.gcsfuse rw 0 0\n", mount_field);

        assert!(is_remote_filesystem_from_mounts(&archive, &mounts).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_mount_detection_uses_longest_mount_point() {
        let mounts = concat!(
            "server:/export /mnt nfs4 rw 0 0\n",
            "/dev/sda1 /mnt/local ext4 rw 0 0\n",
            "s3fs /mnt/s3 fuse.s3fs rw 0 0\n",
            "sshfs#host /home/me/Remote\\040Music fuse.sshfs rw 0 0\n"
        );
        assert!(!is_remote_filesystem_from_mounts(Path::new("/mnt/local/a.zip"), mounts).unwrap());
        assert!(is_remote_filesystem_from_mounts(Path::new("/mnt/remote/a.zip"), mounts).unwrap());
        assert!(is_remote_filesystem_from_mounts(Path::new("/mnt/s3/a.zip"), mounts).unwrap());
        assert!(is_remote_filesystem_from_mounts(Path::new("/home/me/Remote Music/a.zip"), mounts).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_mount_detection_treats_unknown_fuse_as_remote() {
        assert!(is_remote_filesystem_type("fuse.s3fs"));
        assert!(is_remote_filesystem_type("fuse.gcsfuse"));
        assert!(is_remote_filesystem_type("fuse.some-new-remote"));
        assert!(is_remote_filesystem_type("fuse"));
        assert!(!is_remote_filesystem_type("fuseblk"));
    }

    #[test]
    fn cache_key_changes_when_metadata_changes() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        std::fs::write(&archive, b"one").unwrap();
        let first = ArchiveListingCacheKey::for_path(&archive).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        std::fs::write(&archive, b"two two").unwrap();
        let second = ArchiveListingCacheKey::for_path(&archive).unwrap();
        assert_ne!(first, second);
    }

}
