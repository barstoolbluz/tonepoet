// SPDX-License-Identifier: GPL-2.0-or-later
//! Application-level transactional output materialization.
//!
//! Low-level DSF, DSDIFF/DSD, and DSDIFF/DST writers stream to caller-owned
//! `Write + Seek` sinks and deliberately leave partial-output cleanup to the
//! caller. User-facing extraction and conversion paths should instead create an
//! [`OutputTransaction`]: write the whole result to a unique temporary file in
//! the destination directory, commit only after the operation succeeds, and
//! remove the temporary file on every failure path.
//!
//! This module is intentionally format-agnostic so the same safety contract can
//! be used for SACD extraction, DSF/DFF/DST conversion, PCM conversion, and
//! future materializers.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const TEMP_CREATE_ATTEMPTS: u32 = 128;

/// Existing-output policy for transactional materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputOverwritePolicy {
    /// Refuse to create a transaction when the final path already exists.
    RefuseExisting,
    /// Permit replacing the final path, but only after the temporary file has
    /// been fully written and committed.
    ReplaceExisting,
}

impl Default for OutputOverwritePolicy {
    fn default() -> Self {
        Self::RefuseExisting
    }
}

/// Error returned by [`OutputTransaction`] operations.
#[derive(Debug)]
pub enum OutputTransactionError {
    /// The final path has no containing directory.
    MissingParent { final_path: PathBuf },
    /// Existing final output was protected by [`OutputOverwritePolicy::RefuseExisting`].
    ExistingOutputRefused { final_path: PathBuf },
    /// Exhausted unique temporary-name attempts.
    TempNameExhausted { final_path: PathBuf, attempts: u32 },
    /// Temporary-file creation failed.
    CreateTemp {
        final_path: PathBuf,
        temp_path: PathBuf,
        source: io::Error,
    },
    /// A write/seek operation was attempted after the transaction file was
    /// closed by `commit()`/`abort()`.
    Closed {
        final_path: PathBuf,
        temp_path: PathBuf,
    },
    /// Flushing the temporary file before commit failed.
    FlushTemp {
        final_path: PathBuf,
        temp_path: PathBuf,
        source: io::Error,
    },
    /// Syncing the temporary file before commit failed.
    SyncTemp {
        final_path: PathBuf,
        temp_path: PathBuf,
        source: io::Error,
    },
    /// The final rename/replace failed. If cleanup of the temporary file also
    /// failed, that cleanup failure is included explicitly.
    Commit {
        final_path: PathBuf,
        temp_path: PathBuf,
        source: io::Error,
        cleanup: Option<io::Error>,
    },
    /// Explicit abort failed to remove the temporary file.
    CleanupTemp {
        final_path: PathBuf,
        temp_path: PathBuf,
        source: io::Error,
    },
}

impl OutputTransactionError {
    /// Destination path originally requested by the caller.
    pub fn final_path(&self) -> &Path {
        match self {
            Self::MissingParent { final_path }
            | Self::ExistingOutputRefused { final_path }
            | Self::TempNameExhausted { final_path, .. }
            | Self::CreateTemp { final_path, .. }
            | Self::Closed { final_path, .. }
            | Self::FlushTemp { final_path, .. }
            | Self::SyncTemp { final_path, .. }
            | Self::Commit { final_path, .. }
            | Self::CleanupTemp { final_path, .. } => final_path.as_path(),
        }
    }

    /// Temporary path involved in the failure, if one had been selected.
    pub fn temp_path(&self) -> Option<&Path> {
        match self {
            Self::CreateTemp { temp_path, .. }
            | Self::Closed { temp_path, .. }
            | Self::FlushTemp { temp_path, .. }
            | Self::SyncTemp { temp_path, .. }
            | Self::Commit { temp_path, .. }
            | Self::CleanupTemp { temp_path, .. } => Some(temp_path.as_path()),
            Self::MissingParent { .. }
            | Self::ExistingOutputRefused { .. }
            | Self::TempNameExhausted { .. } => None,
        }
    }
}

impl fmt::Display for OutputTransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingParent { final_path } => {
                write!(f, "output transaction: final path {} has no parent directory", final_path.display())
            }
            Self::ExistingOutputRefused { final_path } => {
                write!(f, "output transaction: refusing to overwrite existing output {}", final_path.display())
            }
            Self::TempNameExhausted { final_path, attempts } => write!(
                f,
                "output transaction: could not create a unique temporary name for {} after {} attempts",
                final_path.display(),
                attempts
            ),
            Self::CreateTemp { final_path, temp_path, source } => write!(
                f,
                "output transaction: create temporary file {} for final output {}: {}",
                temp_path.display(),
                final_path.display(),
                source
            ),
            Self::Closed { final_path, temp_path } => write!(
                f,
                "output transaction: temporary file {} for final output {} is already closed",
                temp_path.display(),
                final_path.display()
            ),
            Self::FlushTemp { final_path, temp_path, source } => write!(
                f,
                "output transaction: flush temporary file {} for final output {}: {}",
                temp_path.display(),
                final_path.display(),
                source
            ),
            Self::SyncTemp { final_path, temp_path, source } => write!(
                f,
                "output transaction: sync temporary file {} for final output {}: {}",
                temp_path.display(),
                final_path.display(),
                source
            ),
            Self::Commit { final_path, temp_path, source, cleanup } => {
                write!(
                    f,
                    "output transaction: commit temporary file {} to final output {}: {}",
                    temp_path.display(),
                    final_path.display(),
                    source
                )?;
                if let Some(cleanup) = cleanup {
                    write!(f, "; cleanup of temporary file also failed: {}", cleanup)?;
                }
                Ok(())
            }
            Self::CleanupTemp { final_path, temp_path, source } => write!(
                f,
                "output transaction: remove temporary file {} after failed output {}: {}",
                temp_path.display(),
                final_path.display(),
                source
            ),
        }
    }
}

impl std::error::Error for OutputTransactionError {}

/// A caller-owned output transaction.
///
/// The transaction itself implements [`Write`] and [`Seek`] so existing writers
/// can stream into it directly. Dropping an uncommitted transaction performs
/// best-effort temporary-file cleanup; callers that need diagnostics should call
/// [`Self::abort`] and handle its result.
#[derive(Debug)]
pub struct OutputTransaction {
    final_path: PathBuf,
    temp_path: PathBuf,
    file: Option<File>,
    committed: bool,
    overwrite: OutputOverwritePolicy,
}

impl OutputTransaction {
    /// Create a transaction for `final_path`.
    ///
    /// The temporary file is always created in the final path's destination
    /// directory using `create_new(true)`, which prevents concurrent runs from
    /// colliding or sharing a temp file.
    pub fn create<P: AsRef<Path>>(
        final_path: P,
        overwrite: OutputOverwritePolicy,
    ) -> Result<Self, OutputTransactionError> {
        let final_path = final_path.as_ref().to_path_buf();
        let parent = final_path
            .parent()
            .ok_or_else(|| OutputTransactionError::MissingParent {
                final_path: final_path.clone(),
            })?;

        if final_path.exists() && overwrite == OutputOverwritePolicy::RefuseExisting {
            return Err(OutputTransactionError::ExistingOutputRefused { final_path });
        }

        for attempt in 0..TEMP_CREATE_ATTEMPTS {
            let temp_path = candidate_temp_path(&final_path, attempt);
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&temp_path)
            {
                Ok(file) => {
                    debug_assert_eq!(temp_path.parent(), Some(parent));
                    return Ok(Self {
                        final_path,
                        temp_path,
                        file: Some(file),
                        committed: false,
                        overwrite,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(OutputTransactionError::CreateTemp {
                        final_path,
                        temp_path,
                        source,
                    });
                }
            }
        }

        Err(OutputTransactionError::TempNameExhausted {
            final_path,
            attempts: TEMP_CREATE_ATTEMPTS,
        })
    }

    /// Final destination requested by the caller.
    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    /// Temporary path currently being written.
    pub fn temp_path(&self) -> &Path {
        &self.temp_path
    }

    /// Borrow the underlying temporary file.
    pub fn file_mut(&mut self) -> Result<&mut File, OutputTransactionError> {
        self.file.as_mut().ok_or_else(|| OutputTransactionError::Closed {
            final_path: self.final_path.clone(),
            temp_path: self.temp_path.clone(),
        })
    }

    /// Finish writing and atomically publish the final path where the platform
    /// supports replacement by rename.
    pub fn commit(mut self) -> Result<(), OutputTransactionError> {
        if let Some(mut file) = self.file.take() {
            file.flush().map_err(|source| OutputTransactionError::FlushTemp {
                final_path: self.final_path.clone(),
                temp_path: self.temp_path.clone(),
                source,
            })?;
            file.sync_all().map_err(|source| OutputTransactionError::SyncTemp {
                final_path: self.final_path.clone(),
                temp_path: self.temp_path.clone(),
                source,
            })?;
            drop(file);
        } else {
            return Err(OutputTransactionError::Closed {
                final_path: self.final_path.clone(),
                temp_path: self.temp_path.clone(),
            });
        }

        let rename_result = rename_for_policy(&self.temp_path, &self.final_path, self.overwrite);
        match rename_result {
            Ok(()) => {
                self.committed = true;
                Ok(())
            }
            Err(source) => {
                let cleanup = cleanup_temp(&self.temp_path).err();
                Err(OutputTransactionError::Commit {
                    final_path: self.final_path.clone(),
                    temp_path: self.temp_path.clone(),
                    source,
                    cleanup,
                })
            }
        }
    }

    /// Abort the transaction and remove the temporary file.
    pub fn abort(mut self) -> Result<(), OutputTransactionError> {
        self.file.take();
        match cleanup_temp(&self.temp_path) {
            Ok(()) => {
                self.committed = true; // suppress Drop cleanup; nothing was published
                Ok(())
            }
            Err(source) => Err(OutputTransactionError::CleanupTemp {
                final_path: self.final_path.clone(),
                temp_path: self.temp_path.clone(),
                source,
            }),
        }
    }
}

impl Write for OutputTransaction {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file_mut()
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file_mut()
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?
            .flush()
    }
}

impl Seek for OutputTransaction {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file_mut()
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?
            .seek(pos)
    }
}

impl Drop for OutputTransaction {
    fn drop(&mut self) {
        if !self.committed {
            self.file.take();
            let _ = cleanup_temp(&self.temp_path);
        }
    }
}

fn rename_for_policy(
    temp_path: &Path,
    final_path: &Path,
    overwrite: OutputOverwritePolicy,
) -> io::Result<()> {
    match overwrite {
        OutputOverwritePolicy::RefuseExisting => fs::rename(temp_path, final_path),
        OutputOverwritePolicy::ReplaceExisting => replace_existing(temp_path, final_path),
    }
}

#[cfg(unix)]
fn replace_existing(temp_path: &Path, final_path: &Path) -> io::Result<()> {
    // POSIX rename replaces an existing non-directory destination atomically.
    fs::rename(temp_path, final_path)
}

#[cfg(windows)]
fn replace_existing(temp_path: &Path, final_path: &Path) -> io::Result<()> {
    // `std::fs::rename` does not replace existing files on Windows. Keep the
    // same temp-first safety contract, but use the platform's standard library
    // semantics: conversion has already succeeded, so replacement happens here.
    if final_path.exists() {
        fs::remove_file(final_path)?;
    }
    fs::rename(temp_path, final_path)
}

#[cfg(not(any(unix, windows)))]
fn replace_existing(temp_path: &Path, final_path: &Path) -> io::Result<()> {
    fs::rename(temp_path, final_path)
}

fn cleanup_temp(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn candidate_temp_path(final_path: &Path, attempt: u32) -> PathBuf {
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = final_path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("output");
    let ext = final_path.extension().and_then(|s| s.to_str());
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut name = format!(
        ".{}.tonepoet-tmp.{}.{}.{}.{}",
        stem, pid, counter, nanos, attempt
    );
    if let Some(ext) = ext {
        name.push('.');
        name.push_str(ext);
    }
    parent.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn read_to_string(path: &Path) -> String {
        let mut s = String::new();
        File::open(path).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn success_commits_final_file() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dsf");
        let mut tx = OutputTransaction::create(&final_path, OutputOverwritePolicy::RefuseExisting).unwrap();
        assert_eq!(tx.temp_path().parent(), Some(dir.path()));
        write!(tx, "complete").unwrap();
        let temp = tx.temp_path().to_path_buf();
        tx.commit().unwrap();
        assert_eq!(read_to_string(&final_path), "complete");
        assert!(!temp.exists());
    }

    #[test]
    fn drop_removes_temp_file_on_failure_path() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dff");
        let temp = {
            let mut tx = OutputTransaction::create(&final_path, OutputOverwritePolicy::RefuseExisting).unwrap();
            write!(tx, "partial").unwrap();
            tx.temp_path().to_path_buf()
        };
        assert!(!final_path.exists());
        assert!(!temp.exists());
    }

    #[test]
    fn abort_removes_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dff");
        let mut tx = OutputTransaction::create(&final_path, OutputOverwritePolicy::RefuseExisting).unwrap();
        write!(tx, "partial").unwrap();
        let temp = tx.temp_path().to_path_buf();
        tx.abort().unwrap();
        assert!(!final_path.exists());
        assert!(!temp.exists());
    }

    #[test]
    fn existing_output_is_refused_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dsf");
        fs::write(&final_path, b"old").unwrap();
        let err = OutputTransaction::create(&final_path, OutputOverwritePolicy::RefuseExisting)
            .unwrap_err();
        assert!(matches!(err, OutputTransactionError::ExistingOutputRefused { .. }));
        assert_eq!(read_to_string(&final_path), "old");
    }

    #[test]
    fn existing_output_survives_failed_forced_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dsf");
        fs::write(&final_path, b"old").unwrap();
        let mut tx = OutputTransaction::create(&final_path, OutputOverwritePolicy::ReplaceExisting).unwrap();
        write!(tx, "partial-new").unwrap();
        let temp = tx.temp_path().to_path_buf();
        tx.abort().unwrap();
        assert_eq!(read_to_string(&final_path), "old");
        assert!(!temp.exists());
    }

    #[test]
    fn temp_file_is_created_in_destination_directory_and_preserves_extension() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dff");
        let tx = OutputTransaction::create(&final_path, OutputOverwritePolicy::RefuseExisting).unwrap();
        assert_eq!(tx.temp_path().parent(), Some(dir.path()));
        assert_eq!(tx.temp_path().extension().and_then(|e| e.to_str()), Some("dff"));
    }

    #[test]
    fn repeated_forced_runs_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dsf");
        for _ in 0..3 {
            let mut tx = OutputTransaction::create(&final_path, OutputOverwritePolicy::ReplaceExisting).unwrap();
            write!(tx, "same-bytes").unwrap();
            tx.commit().unwrap();
            assert_eq!(read_to_string(&final_path), "same-bytes");
        }
    }

    #[test]
    fn concurrent_temp_names_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dsf");
        let tx1 = OutputTransaction::create(&final_path, OutputOverwritePolicy::RefuseExisting).unwrap();
        let tx2 = OutputTransaction::create(&final_path, OutputOverwritePolicy::RefuseExisting).unwrap();
        assert_ne!(tx1.temp_path(), tx2.temp_path());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_abort_reports_cleanup_failure() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dsf");
        let tx = OutputTransaction::create(&final_path, OutputOverwritePolicy::RefuseExisting).unwrap();
        let temp = tx.temp_path().to_path_buf();
        fs::remove_file(&temp).unwrap();
        fs::create_dir(&temp).unwrap();
        let err = tx.abort().unwrap_err();
        assert!(matches!(err, OutputTransactionError::CleanupTemp { .. }));
        fs::remove_dir(&temp).unwrap();
    }
}
