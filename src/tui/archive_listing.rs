//! Parse `7zz l -slt` output into structured archive entries.
//!
//! The `-slt` (show technical listing) flag produces key-value pairs for
//! each entry, separated by lines of dashes. This parser extracts the
//! fields needed for browse-like navigation inside archives.

use std::path::{Path, PathBuf};

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
                // Entry is in a subdirectory — register the immediate child dir.
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
            } else if !entry.is_dir {
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

/// List an archive's contents using `7zz l -slt`. Returns the parsed
/// listing or an error string.
pub async fn list_archive(
    archive: &Path,
    password: Option<&str>,
) -> Result<ArchiveListing, String> {
    use tokio::process::Command;

    let bin =
        crate::detect_7z_binary().ok_or_else(|| "neither 7zz nor 7z found in PATH".to_string())?;

    let mut cmd = Command::new(bin);
    cmd.arg("l").arg("-slt").arg(archive);
    if let Some(pw) = password {
        cmd.arg(format!("-p{}", pw));
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to run {}: {}", bin, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Wrong password") || stderr.contains("Cannot open encrypted") {
            return Err("Wrong password".into());
        }
        return Err(format!("{} listing failed: {}", bin, stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_slt_output(&stdout, archive)
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

    /// 7zz v25+ doesn't emit "----------" between entries — only before the
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
}
