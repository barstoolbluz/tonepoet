//! Shared bookmark records and durable TOML persistence.

use crate::text_input::TextInputState;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(unix))]
use std::thread;
#[cfg(not(unix))]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkRecord {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkMoveDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkMutation {
    Add(BookmarkRecord),
    Rename {
        expected_index: usize,
        expected: BookmarkRecord,
        new_name: String,
    },
    Remove {
        expected_index: usize,
        expected: BookmarkRecord,
    },
    Move {
        expected_index: usize,
        expected: BookmarkRecord,
        direction: BookmarkMoveDirection,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkSaveStatus {
    Durable,
    CommittedWithDurabilityWarning { warning: String },
}

impl BookmarkSaveStatus {
    pub fn warning(&self) -> Option<&str> {
        match self {
            Self::Durable => None,
            Self::CommittedWithDurabilityWarning { warning } => Some(warning),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkCommit {
    pub entries: Vec<BookmarkRecord>,
    pub affected_index: usize,
    /// Whether the requested mutation changed the authoritative sequence.
    pub changed: bool,
    pub status: BookmarkSaveStatus,
}

/// Result of a bookmark mutation whose secondary mirror reconciliation ran
/// while the authoritative bookmark lock was still held.
///
/// The TOML commit remains authoritative. A mirror failure is returned
/// separately so callers can adopt the committed entries and surface the
/// secondary failure without pretending that the mutation itself rolled back.
#[derive(Debug)]
pub struct BookmarkReconciledCommit<E> {
    pub commit: BookmarkCommit,
    pub reconcile_result: Result<(), E>,
}

/// Result of a lock-protected first-time store initialization.
///
/// When another process creates the authoritative file before the initializer
/// acquires the lock, `initialized` is false and `entries` contains that
/// process's committed state. The caller must adopt it rather than publishing a
/// stale whole-list snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkInitialization {
    pub entries: Vec<BookmarkRecord>,
    pub initialized: bool,
    pub status: Option<BookmarkSaveStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BookmarkNameAction {
    Add,
    Rename(usize),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BookmarkFile {
    #[serde(default)]
    entries: Vec<BookmarkRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct FilePickerBookmarks {
    pub entries: Vec<BookmarkRecord>,
    pub cursor: usize,
    pub scroll: usize,
    pub naming: Option<BookmarkNameAction>,
    pub name_input: TextInputState,
    pub error: Option<String>,
}

impl Default for FilePickerBookmarks {
    fn default() -> Self {
        match load_bookmarks() {
            Ok(entries) => Self {
                entries,
                cursor: 0,
                scroll: 0,
                naming: None,
                name_input: TextInputState::empty(),
                error: None,
            },
            Err(error) => Self {
                entries: Vec::new(),
                cursor: 0,
                scroll: 0,
                naming: None,
                name_input: TextInputState::empty(),
                error: Some(format!("could not load bookmarks: {error}")),
            },
        }
    }
}

impl FilePickerBookmarks {
    pub fn reload(&mut self) {
        match load_bookmarks() {
            Ok(entries) => {
                self.entries = entries;
                self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
                self.error = None;
            }
            Err(err) => self.error = Some(err.to_string()),
        }
    }

    pub fn ensure_visible(&mut self, visible_rows: usize) {
        let rows = visible_rows.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll.saturating_add(rows) {
            self.scroll = self.cursor.saturating_add(1).saturating_sub(rows);
        }
    }

    pub fn move_cursor(&mut self, delta: isize, visible_rows: usize) {
        if self.entries.is_empty() {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.cursor
                .saturating_add(delta as usize)
                .min(self.entries.len().saturating_sub(1))
        };
        self.ensure_visible(visible_rows);
    }
}

pub fn bookmark_storage_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tonepoet")
        .join("bookmarks.toml")
}

pub fn load_bookmarks() -> io::Result<Vec<BookmarkRecord>> {
    with_bookmark_lock(load_bookmarks_unlocked)
}

fn load_bookmarks_unlocked() -> io::Result<Vec<BookmarkRecord>> {
    let path = bookmark_storage_path();
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bookmark store is not a regular file: {}", path.display()),
            ));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    }
    let text = fs::read_to_string(&path)?;
    let file: BookmarkFile = toml::from_str(&text)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    Ok(file.entries)
}

/// Replace the entire authoritative store while holding the interprocess lock.
///
/// Prefer [`mutate_bookmarks_atomic`] for interactive edits: it reloads the
/// latest committed state under the same lock before applying a mutation, so a
/// standalone picker and the application cannot lose one another's updates.
pub fn save_bookmarks_atomic(entries: &[BookmarkRecord]) -> io::Result<BookmarkSaveStatus> {
    with_bookmark_lock(|| save_bookmarks_atomic_unlocked(entries))
}

/// Initialize the authoritative bookmark store only if it is still absent when
/// the interprocess lock is held.
///
/// This is intended for one-time migration from a compatibility store. It
/// prevents a stale pre-lock existence check from overwriting bookmarks that a
/// picker or another application process committed concurrently.
pub fn initialize_bookmarks_if_absent(
    seed_entries: &[BookmarkRecord],
) -> io::Result<BookmarkInitialization> {
    with_bookmark_lock(|| {
        let path = bookmark_storage_path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "bookmark store is not a regular file: {}",
                            path.display()
                        ),
                    ));
                }
                Ok(BookmarkInitialization {
                    entries: load_bookmarks_unlocked()?,
                    initialized: false,
                    status: None,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let status = save_bookmarks_atomic_unlocked(seed_entries)?;
                Ok(BookmarkInitialization {
                    entries: seed_entries.to_vec(),
                    initialized: true,
                    status: Some(status),
                })
            }
            Err(error) => Err(error),
        }
    })
}

/// Apply one bookmark mutation against the latest authoritative state under an
/// interprocess lock, then atomically publish the resulting file.
pub fn mutate_bookmarks_atomic(mutation: BookmarkMutation) -> io::Result<BookmarkCommit> {
    with_bookmark_lock(|| mutate_bookmarks_unlocked(mutation))
}

/// Apply one bookmark mutation and reconcile a secondary mirror before the
/// authoritative interprocess lock is released.
///
/// `reconcile` receives the latest committed TOML sequence and whether the
/// requested mutation changed that sequence. Keeping the callback inside the
/// same lock closes the stale-writer window between an authoritative commit and
/// mirror replacement. The callback should make its own replacement atomic
/// (for example, with one SQLite transaction).
pub fn mutate_bookmarks_atomic_with_reconcile<E>(
    mutation: BookmarkMutation,
    reconcile: impl FnOnce(&[BookmarkRecord], bool) -> Result<(), E>,
) -> io::Result<BookmarkReconciledCommit<E>> {
    with_bookmark_lock(|| {
        let commit = mutate_bookmarks_unlocked(mutation)?;
        let reconcile_result = reconcile(&commit.entries, commit.changed);
        Ok(BookmarkReconciledCommit {
            commit,
            reconcile_result,
        })
    })
}

/// Reload the authoritative sequence and reconcile a secondary mirror while
/// the bookmark interprocess lock remains held.
///
/// This is used for startup/repair paths that do not themselves mutate TOML but
/// must not race a newer mutation while replacing the mirror.
pub fn reconcile_bookmarks_locked<E>(
    reconcile: impl FnOnce(&[BookmarkRecord]) -> Result<(), E>,
) -> io::Result<(Vec<BookmarkRecord>, Result<(), E>)> {
    with_bookmark_lock(|| {
        let entries = load_bookmarks_unlocked()?;
        let result = reconcile(&entries);
        Ok((entries, result))
    })
}

fn mutate_bookmarks_unlocked(mutation: BookmarkMutation) -> io::Result<BookmarkCommit> {
    let mut entries = load_bookmarks_unlocked()?;
    let (affected_index, changed) = apply_mutation(&mut entries, mutation)?;
    let status = if changed {
        save_bookmarks_atomic_unlocked(&entries)?
    } else {
        BookmarkSaveStatus::Durable
    };
    Ok(BookmarkCommit {
        entries,
        affected_index,
        changed,
        status,
    })
}

fn apply_mutation(
    entries: &mut Vec<BookmarkRecord>,
    mutation: BookmarkMutation,
) -> io::Result<(usize, bool)> {
    let (affected_index, changed) = match mutation {
        BookmarkMutation::Add(entry) => {
            entries.push(entry);
            (entries.len().saturating_sub(1), true)
        }
        BookmarkMutation::Rename {
            expected_index,
            expected,
            new_name,
        } => {
            let index = resolve_expected_entry(entries, expected_index, &expected)?;
            let changed = entries[index].name != new_name;
            entries[index].name = new_name;
            (index, changed)
        }
        BookmarkMutation::Remove {
            expected_index,
            expected,
        } => {
            let index = resolve_expected_entry(entries, expected_index, &expected)?;
            entries.remove(index);
            (index.min(entries.len().saturating_sub(1)), true)
        }
        BookmarkMutation::Move {
            expected_index,
            expected,
            direction,
        } => {
            let index = resolve_expected_entry(entries, expected_index, &expected)?;
            let destination = match direction {
                BookmarkMoveDirection::Up => index.saturating_sub(1),
                BookmarkMoveDirection::Down => {
                    (index + 1).min(entries.len().saturating_sub(1))
                }
            };
            let changed = destination != index;
            if changed {
                entries.swap(index, destination);
            }
            (destination, changed)
        }
    };
    Ok((affected_index, changed))
}

fn resolve_expected_entry(
    entries: &[BookmarkRecord],
    expected_index: usize,
    expected: &BookmarkRecord,
) -> io::Result<usize> {
    if entries.get(expected_index) == Some(expected) {
        return Ok(expected_index);
    }

    let mut matches = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| (entry == expected).then_some(index));
    let Some(index) = matches.next() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "bookmark changed or was removed by another process",
        ));
    };
    if matches.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bookmark changed concurrently and is now ambiguous",
        ));
    }
    Ok(index)
}

fn save_bookmarks_atomic_unlocked(entries: &[BookmarkRecord]) -> io::Result<BookmarkSaveStatus> {
    let path = bookmark_storage_path();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let document = toml::to_string_pretty(&BookmarkFile {
        entries: entries.to_vec(),
    })
    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

    let (temporary, mut file) = create_unique_temporary(parent)?;
    if let Err(err) = file
        .write_all(document.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(&temporary);
        return Err(err);
    }
    drop(file);

    if let Err(err) = atomic_replace(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(err);
    }

    // The rename is the visibility/commit point. A directory fsync failure
    // means the new state is already authoritative in this running system but
    // crash durability could not be proven. Report that distinctly; callers
    // must not roll back their in-memory state after this point.
    match sync_parent_directory(parent) {
        Ok(()) => Ok(BookmarkSaveStatus::Durable),
        Err(err) => Ok(BookmarkSaveStatus::CommittedWithDurabilityWarning {
            warning: format!(
                "bookmark update committed, but parent-directory synchronization failed: {err}"
            ),
        }),
    }
}

fn create_unique_temporary(parent: &Path) -> io::Result<(PathBuf, fs::File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for _ in 0..128 {
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".bookmarks.toml.tmp-{}-{timestamp}-{counter}",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique bookmarks temporary file",
    ))
}

#[cfg(unix)]
struct BookmarkLock {
    file: fs::File,
}

#[cfg(unix)]
impl Drop for BookmarkLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(unix)]
fn acquire_bookmark_lock(parent: &Path) -> io::Result<BookmarkLock> {
    use std::os::fd::AsRawFd;

    fs::create_dir_all(parent)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(parent.join(".bookmarks.lock"))?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if result == 0 {
        Ok(BookmarkLock { file })
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
struct BookmarkLock {
    path: PathBuf,
}

#[cfg(not(unix))]
impl Drop for BookmarkLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

#[cfg(not(unix))]
fn acquire_bookmark_lock(parent: &Path) -> io::Result<BookmarkLock> {
    fs::create_dir_all(parent)?;
    let path = parent.join(".bookmarks.lockdir");
    for _ in 0..250 {
        match fs::create_dir(&path) {
            Ok(()) => return Ok(BookmarkLock { path }),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "timed out waiting for the bookmark store lock",
    ))
}

fn with_bookmark_lock<T>(operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let path = bookmark_storage_path();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let _lock = acquire_bookmark_lock(parent)?;
    operation()
}

#[cfg(unix)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PATH_NOT_FOUND: i32 = 3;
    const ERROR_FILE_EXISTS: i32 = 80;
    const ERROR_ALREADY_EXISTS: i32 = 183;

    #[link(name = "kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    let source_wide = wide(source);
    let destination_wide = wide(destination);
    let replace = || unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            source_wide.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if replace() != 0 {
        return Ok(());
    }
    let replace_error = io::Error::last_os_error();
    match replace_error.raw_os_error() {
        Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PATH_NOT_FOUND) => {
            let moved = unsafe {
                MoveFileExW(
                    source_wide.as_ptr(),
                    destination_wide.as_ptr(),
                    MOVEFILE_WRITE_THROUGH,
                )
            };
            if moved != 0 {
                return Ok(());
            }
            let move_error = io::Error::last_os_error();
            if matches!(
                move_error.raw_os_error(),
                Some(ERROR_FILE_EXISTS) | Some(ERROR_ALREADY_EXISTS)
            ) && replace() != 0
            {
                return Ok(());
            }
            Err(move_error)
        }
        _ => Err(replace_error),
    }
}

#[cfg(not(any(unix, windows)))]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "parent-directory durability synchronization is unavailable for {} on this platform",
            parent.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmark_toml_schema_is_compatible_with_app_store() {
        let text = toml::to_string(&BookmarkFile {
            entries: vec![BookmarkRecord {
                name: "Music".to_string(),
                path: PathBuf::from("/music"),
            }],
        })
        .expect("serialize");
        assert!(text.contains("[[entries]]"));
        assert!(text.contains("name = \"Music\""));
    }

    #[test]
    fn mutation_relocates_unique_concurrent_entry_by_identity() {
        let expected = BookmarkRecord {
            name: "Music".to_string(),
            path: PathBuf::from("/music"),
        };
        let mut entries = vec![
            BookmarkRecord {
                name: "Other".to_string(),
                path: PathBuf::from("/other"),
            },
            expected.clone(),
        ];
        apply_mutation(
            &mut entries,
            BookmarkMutation::Rename {
                expected_index: 0,
                expected,
                new_name: "Library".to_string(),
            },
        )
        .expect("rename");
        assert_eq!(entries[1].name, "Library");
    }

    #[test]
    fn rename_mutation_reports_successful_no_op_without_rewrite_intent() {
        let expected = BookmarkRecord {
            name: "Music".to_string(),
            path: PathBuf::from("/music"),
        };
        let mut entries = vec![expected.clone()];
        let result = apply_mutation(
            &mut entries,
            BookmarkMutation::Rename {
                expected_index: 0,
                expected: expected.clone(),
                new_name: expected.name.clone(),
            },
        )
        .expect("rename no-op");
        assert_eq!(result, (0, false));
        assert_eq!(entries, vec![expected]);
    }

    #[test]
    fn mutation_rejects_ambiguous_concurrent_duplicates() {
        let expected = BookmarkRecord {
            name: "Music".to_string(),
            path: PathBuf::from("/music"),
        };
        let mut entries = vec![expected.clone(), expected.clone()];
        let error = apply_mutation(
            &mut entries,
            BookmarkMutation::Remove {
                expected_index: 9,
                expected,
            },
        )
        .expect_err("ambiguous mutation must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn move_mutation_relocates_unique_entry_then_swaps() {
        let expected = BookmarkRecord {
            name: "Music".to_string(),
            path: PathBuf::from("/music"),
        };
        let mut entries = vec![
            BookmarkRecord {
                name: "Inserted".to_string(),
                path: PathBuf::from("/inserted"),
            },
            expected.clone(),
            BookmarkRecord {
                name: "Downloads".to_string(),
                path: PathBuf::from("/downloads"),
            },
        ];
        let affected = apply_mutation(
            &mut entries,
            BookmarkMutation::Move {
                expected_index: 0,
                expected,
                direction: BookmarkMoveDirection::Down,
            },
        )
        .expect("move");
        assert_eq!(affected, (2, true));
        assert_eq!(entries[2].name, "Music");
    }

    #[test]
    fn move_mutation_is_idempotent_at_boundaries() {
        let first = BookmarkRecord {
            name: "First".to_string(),
            path: PathBuf::from("/first"),
        };
        let last = BookmarkRecord {
            name: "Last".to_string(),
            path: PathBuf::from("/last"),
        };
        let mut entries = vec![first.clone(), last.clone()];
        assert_eq!(
            apply_mutation(
                &mut entries,
                BookmarkMutation::Move {
                    expected_index: 0,
                    expected: first.clone(),
                    direction: BookmarkMoveDirection::Up,
                },
            )
            .expect("top no-op"),
            (0, false)
        );
        assert_eq!(
            apply_mutation(
                &mut entries,
                BookmarkMutation::Move {
                    expected_index: 1,
                    expected: last.clone(),
                    direction: BookmarkMoveDirection::Down,
                },
            )
            .expect("bottom no-op"),
            (1, false)
        );
        assert_eq!(entries, vec![first, last]);
    }

    #[test]
    fn moved_order_survives_toml_round_trip() {
        let first = BookmarkRecord {
            name: "First".to_string(),
            path: PathBuf::from("/first"),
        };
        let second = BookmarkRecord {
            name: "Second".to_string(),
            path: PathBuf::from("/second"),
        };
        let third = BookmarkRecord {
            name: "Third".to_string(),
            path: PathBuf::from("/third"),
        };
        let mut entries = vec![first, second.clone(), third];
        apply_mutation(
            &mut entries,
            BookmarkMutation::Move {
                expected_index: 1,
                expected: second,
                direction: BookmarkMoveDirection::Up,
            },
        )
        .expect("move");

        let serialized = toml::to_string(&BookmarkFile {
            entries: entries.clone(),
        })
        .expect("serialize");
        let reparsed: BookmarkFile = toml::from_str(&serialized).expect("parse");

        assert_eq!(reparsed.entries, entries);
        assert_eq!(
            reparsed
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Second", "First", "Third"]
        );
    }

}
