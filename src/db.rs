//! SQLite database: core persistence layer for tonepoet.
//!
//! Database at `~/.local/share/tonepoet/tonepoet.db` (XDG_DATA_HOME).
//! WAL mode enabled for crash safety. Schema versioned via PRAGMA
//! user_version with forward migrations on open.

use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

/// Schema version — bump when adding migrations.
const CURRENT_VERSION: u32 = 23;

const LEGACY_IMPORT_STATE_ROW_ID: i64 = 1;
pub(crate) const RECENT_FILES_RETENTION_LIMIT: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyImportPublication {
    Imported,
    AlreadyDone,
    ExistingSqliteAuthority,
}

#[derive(Debug)]
pub struct QueueLoadOutcome {
    pub items: Vec<crate::convert::ConversionItem>,
    /// A non-fatal persistence error that occurred after the authoritative
    /// rows were read. The caller must surface this as degraded persistence
    /// while still presenting `items`; hiding salvageable work would turn a
    /// maintenance-write failure into an apparent empty queue.
    pub degradation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueSyncReport {
    pub rows_written: usize,
    pub rows_deleted: usize,
    /// References removed or superseded by the committed SQLite mutation.
    /// Callers that can still have live workers must defer retirement for any
    /// reference those workers still own.
    pub retire_references: Vec<String>,
    /// Queue-owned references that remain durably reachable from the rows
    /// published by this transaction. Callers use this to avoid retiring a
    /// reference that was removed and then deliberately reused before the
    /// persistence boundary completed.
    pub live_references: Vec<String>,
}

// ── CTDB parity matrix cache tunables ─────────────────────────────────
//
// Each cached entry is `STRIDE * NPAR * 2` bytes — for STRIDE=11_760, NPAR=16
// that's 376_320 bytes per disc. With CTDB_PARITY_CACHE_MAX_ROWS=2000 the
// upper bound is roughly 750 MB. Eviction trips when the row count exceeds
// CTDB_PARITY_CACHE_EVICT_THRESHOLD (110% of the cap) and trims down to
// CTDB_PARITY_CACHE_EVICT_TARGET (90% of the cap), removing the
// least-recently-used rows by `accessed_at`. Batch eviction amortizes
// the deletion cost across many cache stores.

const CTDB_PARITY_CACHE_MAX_ROWS: usize = 2000;
const CTDB_PARITY_CACHE_EVICT_THRESHOLD: usize = (CTDB_PARITY_CACHE_MAX_ROWS * 110) / 100;
const CTDB_PARITY_CACHE_EVICT_TARGET: usize = (CTDB_PARITY_CACHE_MAX_ROWS * 90) / 100;

// MusicBrainz TOC lookup cache. Each row stores a JSON release blob keyed by
// disc TOC string. 30-day TTL; LRU eviction when row count exceeds threshold.
const MB_CACHE_MAX_ROWS: usize = 5000;
const MB_CACHE_EVICT_THRESHOLD: usize = (MB_CACHE_MAX_ROWS * 110) / 100;
const MB_CACHE_EVICT_TARGET: usize = (MB_CACHE_MAX_ROWS * 90) / 100;
const MB_CACHE_TTL_SECS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingArchiveSessionRecovery {
    pub archive_path: PathBuf,
    pub staging_dir: PathBuf,
    pub archive_mtime_secs: i64,
    pub archive_mtime_nanos: u32,
    pub archive_size: u64,
    pub edits_json: String,
    pub conflicted: bool,
    pub conflict_reason: Option<String>,
}


// MusicBrainz text-search + release-detail cache (Phase B-5). Keys are
// canonical query strings produced by the musicbrainz module
// (`search_cache_key` / `detail_cache_key`). Same shape and TTL as the
// TOC cache; separate table so eviction policies stay independent.
const MB_SEARCH_CACHE_MAX_ROWS: usize = 5000;
const MB_SEARCH_CACHE_EVICT_THRESHOLD: usize = (MB_SEARCH_CACHE_MAX_ROWS * 110) / 100;
const MB_SEARCH_CACHE_EVICT_TARGET: usize = (MB_SEARCH_CACHE_MAX_ROWS * 90) / 100;

/// Core database wrapper. Owns a single SQLite connection.
pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetadataJournalEntry {
    file_path: String,
    backup_path: String,
    started_at: String,
    state: String,
}

const METADATA_STATE_ALLOCATING: &str = "allocating";
const METADATA_STATE_PREPARED: &str = "prepared";
const METADATA_STATE_COMMITTED: &str = "committed";
const METADATA_STATE_ROLLED_BACK: &str = "rolled_back";

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TestMetadataMutationAudit {
    pub journal_write_transactions: u64,
    pub backup_bytes_copied: u64,
}

#[cfg(test)]
thread_local! {
    static TEST_METADATA_MUTATION_AUDIT: std::cell::RefCell<TestMetadataMutationAudit> =
        std::cell::RefCell::new(TestMetadataMutationAudit::default());
}

#[cfg(test)]
pub(crate) fn reset_test_metadata_mutation_audit() {
    TEST_METADATA_MUTATION_AUDIT.with(|audit| {
        *audit.borrow_mut() = TestMetadataMutationAudit::default();
    });
}

#[cfg(test)]
pub(crate) fn test_metadata_mutation_audit() -> TestMetadataMutationAudit {
    TEST_METADATA_MUTATION_AUDIT.with(|audit| *audit.borrow())
}

#[cfg(test)]
fn record_test_metadata_journal_write() {
    TEST_METADATA_MUTATION_AUDIT.with(|audit| {
        audit.borrow_mut().journal_write_transactions += 1;
    });
}

#[cfg(not(test))]
fn record_test_metadata_journal_write() {}

#[cfg(test)]
fn record_test_metadata_backup_copy(bytes: u64) {
    TEST_METADATA_MUTATION_AUDIT.with(|audit| {
        audit.borrow_mut().backup_bytes_copied += bytes;
    });
}

#[cfg(not(test))]
fn record_test_metadata_backup_copy(_bytes: u64) {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabasePragmaProfile {
    FileBacked,
    InMemory,
}

const DB_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const DB_OPEN_INIT_LOCK_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
const DB_OPEN_INIT_LOCK_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

struct DatabaseOpenInitFileLock(std::fs::File);

impl Drop for DatabaseOpenInitFileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

/// Return the database file path.
pub fn db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tonepoet")
        .join("tonepoet.db")
}

fn path_has_component_with_prefix(path: &std::path::Path, prefix: &str) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .starts_with(prefix)
    })
}

fn path_file_name_starts_with(path: &std::path::Path, prefix: &str) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy().starts_with(prefix))
        .unwrap_or(false)
}

fn looks_like_archive_staging_dir(path: &std::path::Path) -> bool {
    path_file_name_starts_with(path, "tonepoet-archive-metadata-")
        || path_file_name_starts_with(path, "tonepoet-archive-rename-")
        || path_file_name_starts_with(path, "tonepoet-archive-delete-")
}

fn looks_like_test_archive_session_artifact(
    archive_path: &std::path::Path,
    staging_dir: &std::path::Path,
) -> bool {
    if !looks_like_archive_staging_dir(staging_dir)
        || !path_has_component_with_prefix(staging_dir, "nix-shell.")
    {
        return false;
    }

    // Test fixtures created by tempfile::tempdir() inside a nix dev shell look
    // like `/tmp/nix-shell.XXXX/.tmpYYYY/album.zip`, while their archive-edit
    // staging directory is a sibling `/tmp/nix-shell.XXXX/tonepoet-archive-*`.
    // These rows are not recoverable user state and should never drive the
    // startup recovery dialog, even while the nix shell keeps /tmp alive.
    let archive_is_tempfile_fixture = path_has_component_with_prefix(archive_path, "nix-shell.")
        && path_has_component_with_prefix(archive_path, ".tmp");
    let archive_is_missing_nix_shell_temp = !archive_path.exists()
        && path_has_component_with_prefix(archive_path, "nix-shell.");

    archive_is_tempfile_fixture || archive_is_missing_nix_shell_temp
}

impl Database {
    /// Open (or create) the production database, run migrations, and enable the
    /// same durability/performance pragmas used by ordinary application starts.
    pub fn open() -> Result<Self, String> {
        Self::open_path(db_path())
    }

    /// Open (or create) a database at an explicit path.
    ///
    /// This is intentionally available outside `cfg(test)` so integration tests
    /// and harnesses that link the crate as a normal dependency can inject a
    /// per-test SQLite file instead of touching the user's XDG production DB.
    /// It also exercises production-like behavior that an in-memory connection
    /// cannot: parent directory creation, file-backed locking, WAL sidecars, and
    /// migrations against a persistent database file.
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create DB directory {}: {e}", parent.display()))?;
        }

        // Routine opens take shared initialization authority. Multiple
        // processes may probe/open an established database concurrently, while
        // an exclusive first-open or migration window blocks those probes until
        // persistent WAL/schema state is ready.
        let shared_init = Self::acquire_open_init_file_lock(path, false)?;
        let existing_nonempty_file = match std::fs::metadata(path) {
            Ok(metadata) => metadata.is_file() && metadata.len() > 0,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(format!(
                    "failed to inspect database path {}: {error}",
                    path.display()
                ));
            }
        };
        if existing_nonempty_file {
            let conn = Connection::open(path)
                .map_err(|e| format!("failed to open database {}: {e}", path.display()))?;
            conn.busy_timeout(DB_BUSY_TIMEOUT)
                .map_err(|e| format!("busy_timeout pragma failed: {}", e))?;
            if Self::file_backed_database_is_initialized(&conn)? {
                Self::configure_file_backed_connection(&conn)?;
                return Ok(Self { conn });
            }
        }
        drop(shared_init);

        // Slow-path initialization upgrades by releasing shared authority and
        // reacquiring exclusive authority. Recheck all persistent state after
        // the exclusive lock because another process may win the upgrade race.
        // The process mutex avoids platform-specific same-process lock quirks
        // while the file lock extends authority across tonepoet processes.
        static DB_OPEN_INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _process_init = DB_OPEN_INIT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _exclusive_init = Self::acquire_open_init_file_lock(path, true)?;
        let conn = Connection::open(path)
            .map_err(|e| format!("failed to open database {}: {e}", path.display()))?;
        Self::from_connection(conn, DatabasePragmaProfile::FileBacked)
    }

    fn open_init_lock_path(path: &Path) -> PathBuf {
        let mut lock_name = path.as_os_str().to_os_string();
        lock_name.push(".open-init.lock");
        PathBuf::from(lock_name)
    }

    fn acquire_open_init_file_lock(
        path: &Path,
        exclusive: bool,
    ) -> Result<DatabaseOpenInitFileLock, String> {
        let lock_path = Self::open_init_lock_path(path);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&lock_path)
            .map_err(|error| {
                format!(
                    "failed to open database initialization lock {}: {error}",
                    lock_path.display()
                )
            })?;
        let started = std::time::Instant::now();
        loop {
            let result = if exclusive {
                fs2::FileExt::try_lock_exclusive(&file)
            } else {
                fs2::FileExt::try_lock_shared(&file)
            };
            match result {
                Ok(()) => return Ok(DatabaseOpenInitFileLock(file)),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if Self::is_file_lock_contention(&error) => {
                    if started.elapsed() >= DB_OPEN_INIT_LOCK_WAIT_LIMIT {
                        let mode = if exclusive { "exclusive" } else { "shared" };
                        return Err(format!(
                            "timed out after {} ms waiting for {mode} database initialization lock {}",
                            DB_OPEN_INIT_LOCK_WAIT_LIMIT.as_millis(),
                            lock_path.display()
                        ));
                    }
                    std::thread::sleep(DB_OPEN_INIT_LOCK_RETRY_DELAY);
                }
                Err(error) => {
                    let mode = if exclusive { "exclusive" } else { "shared" };
                    return Err(format!(
                        "failed to take {mode} database initialization lock {}: {error}",
                        lock_path.display()
                    ));
                }
            }
        }
    }

    fn is_file_lock_contention(error: &std::io::Error) -> bool {
        error.kind() == fs2::lock_contended_error().kind()
    }

    fn file_backed_database_is_initialized(conn: &Connection) -> Result<bool, String> {
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(|error| format!("read journal_mode during database open: {error}"))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Ok(false);
        }
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| format!("read user_version during database open: {error}"))?;
        Ok(version == CURRENT_VERSION)
    }

    fn configure_file_backed_connection(conn: &Connection) -> Result<(), String> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|e| format!("foreign_keys pragma failed: {}", e))?;
        // Browse performs many small cache reads during navigation. A larger
        // page cache and mmap window reduce syscall churn without changing
        // schema or transaction semantics; unsupported platforms clamp
        // mmap_size to SQLite's accepted value.
        conn.execute_batch("PRAGMA cache_size = -65536; PRAGMA mmap_size = 268435456;")
            .map_err(|e| format!("performance pragmas failed: {}", e))?;
        Ok(())
    }

    fn enable_wal_mode(conn: &Connection) -> Result<(), String> {
        let current_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(|error| format!("read journal_mode before WAL initialization: {error}"))?;
        if current_mode.eq_ignore_ascii_case("wal") {
            return Ok(());
        }

        match conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get::<_, String>(0)) {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => Ok(()),
            Ok(mode) => Err(format!(
                "WAL pragma did not activate WAL mode; SQLite reported journal_mode={mode:?}"
            )),
            Err(error) => {
                // A process from an older tonepoet build may race this build and
                // finish the same persistent mode switch after our statement
                // reports contention. Accept only a verified WAL result; retain
                // every other I/O/locking failure so storage faults stay visible.
                match conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0)) {
                    Ok(mode) if mode.eq_ignore_ascii_case("wal") => Ok(()),
                    Ok(mode) => Err(format!(
                        "WAL pragma failed: {error}; journal_mode remained {mode:?}"
                    )),
                    Err(recheck_error) => Err(format!(
                        "WAL pragma failed: {error}; journal_mode recheck also failed: {recheck_error}"
                    )),
                }
            }
        }
    }

    /// Open an in-memory database (for explicit lightweight tests and fallback).
    /// Prefer `open_path` for tests that should cover file-backed SQLite/WAL
    /// semantics.
    pub fn open_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory()
            .map_err(|e| format!("failed to open in-memory DB: {}", e))?;
        Self::from_connection(conn, DatabasePragmaProfile::InMemory)
    }

    fn from_connection(
        conn: Connection,
        profile: DatabasePragmaProfile,
    ) -> Result<Self, String> {
        match profile {
            DatabasePragmaProfile::FileBacked => {
                // Wait (rather than immediately failing with "database is
                // locked") when another connection briefly holds the lock. This
                // must precede the WAL pragma: switching journal_mode to WAL
                // takes a transient exclusive lock, and on a freshly created DB
                // with rapid successive opens (e.g. multi-file metadata writes)
                // that lock can momentarily collide. Without a busy timeout the
                // default is zero and the very first write fails.
                conn.busy_timeout(DB_BUSY_TIMEOUT)
                    .map_err(|e| format!("busy_timeout pragma failed: {}", e))?;
                // WAL is persistent for a file-backed database. Read the mode on
                // ordinary opens and mutate it only when initialization actually
                // needs a mode switch. This avoids taking a journal-mode lock on
                // every metadata write while still validating the required mode.
                Self::enable_wal_mode(&conn)?;
                Self::configure_file_backed_connection(&conn)?;
            }
            DatabasePragmaProfile::InMemory => {
                conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA cache_size = -32768;")
                    .map_err(|e| format!("in-memory pragmas failed: {}", e))?;
            }
        }

        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Run forward migrations up to CURRENT_VERSION.
    fn migrate(&mut self) -> Result<(), String> {
        let mut version: u32 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|e| format!("read user_version: {}", e))?;

        if version > CURRENT_VERSION {
            return Err(format!(
                "database schema is newer than this build (found {}, supports {})",
                version, CURRENT_VERSION
            ));
        }
        if version == CURRENT_VERSION {
            return Ok(());
        }

        if version < 1 {
            self.run_migration_step(1, Self::migrate_v1)?;
            version = 1;
        }
        if version < 2 {
            self.run_migration_step(2, Self::migrate_v2)?;
            version = 2;
        }
        if version < 3 {
            self.run_migration_step(3, Self::migrate_v3)?;
            version = 3;
        }
        if version < 4 {
            self.run_migration_step(4, Self::migrate_v4)?;
            version = 4;
        }
        if version < 5 {
            self.run_migration_step(5, Self::migrate_v5)?;
            version = 5;
        }
        if version < 6 {
            self.run_migration_step(6, Self::migrate_v6)?;
            version = 6;
        }
        if version < 7 {
            self.run_migration_step(7, Self::migrate_v7)?;
            version = 7;
        }
        if version < 8 {
            self.run_migration_step(8, Self::migrate_v8)?;
            version = 8;
        }
        if version < 9 {
            self.run_migration_step(9, Self::migrate_v9)?;
            version = 9;
        }
        if version < 10 {
            self.run_migration_step(10, Self::migrate_v10)?;
            version = 10;
        }
        if version < 11 {
            self.run_migration_step(11, Self::migrate_v11)?;
            version = 11;
        }
        if version < 12 {
            self.run_migration_step(12, Self::migrate_v12)?;
            version = 12;
        }
        if version < 13 {
            self.run_migration_step(13, Self::migrate_v13)?;
            version = 13;
        }
        if version < 14 {
            self.run_migration_step(14, Self::migrate_v14)?;
            version = 14;
        }
        if version < 15 {
            self.run_migration_step(15, Self::migrate_v15)?;
            version = 15;
        }
        if version < 16 {
            self.run_migration_step(16, Self::migrate_v16)?;
            version = 16;
        }
        if version < 17 {
            self.run_migration_step(17, Self::migrate_v17)?;
            version = 17;
        }
        if version < 18 {
            self.run_migration_step(18, Self::migrate_v18)?;
            version = 18;
        }
        if version < 19 {
            self.run_migration_step(19, Self::migrate_v19)?;
            version = 19;
        }
        if version < 20 {
            self.run_migration_step(20, Self::migrate_v20)?;
            version = 20;
        }
        if version < 21 {
            self.run_migration_step(21, Self::migrate_v21)?;
            version = 21;
        }
        if version < 22 {
            self.run_migration_step(22, Self::migrate_v22)?;
            version = 22;
        }
        if version < 23 {
            self.run_migration_step(23, Self::migrate_v23)?;
        }

        Ok(())
    }

    fn run_migration_step(
        &mut self,
        version: u32,
        migration: fn(&Connection) -> Result<(), String>,
    ) -> Result<(), String> {
        let transaction = self
            .conn
            .transaction()
            .map_err(|e| format!("begin v{} migration: {}", version, e))?;

        migration(&transaction)?;
        transaction
            .pragma_update(None, "user_version", version)
            .map_err(|e| format!("set user_version to {}: {}", version, e))?;
        transaction
            .commit()
            .map_err(|e| format!("commit v{} migration: {}", version, e))?;

        Ok(())
    }

    // Narrow recovery helper for historical ADD COLUMN migrations that could
    // have committed before user_version advanced. This is intentionally not a
    // general schema reconciliation mechanism.
    fn legacy_add_column_present(
        conn: &Connection,
        migration: &str,
        table: &str,
        column: &str,
        expected_type: &str,
        expected_not_null: bool,
        expected_default: Option<&str>,
    ) -> Result<bool, String> {
        let pragma = format!("PRAGMA table_info({})", table);
        let mut columns = conn
            .prepare(&pragma)
            .map_err(|e| format!("{} migration inspect {}: {}", migration, table, e))?;
        let rows = columns
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|e| format!("{} migration inspect {}: {}", migration, table, e))?;

        for row in rows {
            let (name, declared_type, not_null, default_value) = row
                .map_err(|e| format!("{} migration decode {} column: {}", migration, table, e))?;
            if name != column {
                continue;
            }

            let type_matches = declared_type.trim().eq_ignore_ascii_case(expected_type);
            let default_matches = default_value.as_deref() == expected_default;
            if type_matches && not_null == expected_not_null && default_matches {
                return Ok(true);
            }

            let expected_default = expected_default.unwrap_or("<none>");
            let actual_default = default_value.as_deref().unwrap_or("<none>");
            return Err(format!(
                "{} migration found incompatible existing column {}.{}: expected type {} NOT NULL={} DEFAULT {}, found type {} NOT NULL={} DEFAULT {}",
                migration,
                table,
                column,
                expected_type,
                expected_not_null,
                expected_default,
                declared_type,
                not_null,
                actual_default
            ));
        }

        Ok(false)
    }

    /// v1: metadata journal, probe cache, recent files, bookmarks.
    fn migrate_v1(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS metadata_journal (
                file_path   TEXT PRIMARY KEY,
                backup_path TEXT NOT NULL,
                started_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS probe_cache (
                file_path       TEXT PRIMARY KEY,
                file_mtime      INTEGER NOT NULL,
                file_size       INTEGER NOT NULL,
                format_name     TEXT,
                codec           TEXT,
                bit_depth       INTEGER,
                sample_rate     INTEGER,
                channels        INTEGER,
                channel_layout  TEXT,
                duration_secs   REAL,
                title           TEXT,
                artist          TEXT,
                album           TEXT,
                genre           TEXT,
                year            TEXT,
                track_number    INTEGER,
                catalog_number  TEXT,
                rg_track_gain   TEXT,
                rg_track_peak   TEXT,
                rg_album_gain   TEXT,
                rg_album_peak   TEXT,
                r128_track_gain TEXT,
                r128_album_gain TEXT,
                probed_at       TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS recent_files (
                file_path   TEXT PRIMARY KEY,
                accessed_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS bookmarks (
                id       INTEGER PRIMARY KEY,
                name     TEXT NOT NULL,
                path     TEXT NOT NULL,
                position INTEGER NOT NULL
            );
        ",
            )
            .map_err(|e| format!("v1 migration failed: {}", e))?;

        Ok(())
    }

    /// v2: presets table.
    fn migrate_v2(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS presets (
                name            TEXT PRIMARY KEY,
                description     TEXT,
                format          TEXT NOT NULL,
                sample_rate     INTEGER,
                bit_depth       TEXT,
                dither          TEXT,
                replaygain      TEXT,
                folder_template TEXT,
                filename_template TEXT,
                merge           TEXT,
                version         INTEGER NOT NULL DEFAULT 2
            );
            CREATE INDEX IF NOT EXISTS idx_presets_format ON presets(format);
        ",
            )
            .map_err(|e| format!("v2 migration failed: {}", e))?;
        Ok(())
    }

    /// v3: conversion history table.
    fn migrate_v3(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS conversion_history (
                id              INTEGER PRIMARY KEY,
                input_path      TEXT NOT NULL,
                output_path     TEXT,
                input_format    TEXT,
                output_format   TEXT NOT NULL,
                sample_rate     INTEGER,
                bit_depth       TEXT,
                dither          TEXT,
                replaygain_mode TEXT,
                source_size     INTEGER,
                output_size     INTEGER,
                queued_at       TEXT,
                started_at      TEXT,
                completed_at    TEXT NOT NULL,
                success         INTEGER NOT NULL,
                error_message   TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_history_completed
                ON conversion_history(completed_at);
            CREATE INDEX IF NOT EXISTS idx_history_input
                ON conversion_history(input_path);
        ",
            )
            .map_err(|e| format!("v3 migration failed: {}", e))?;
        Ok(())
    }

    /// v4: batch state table for Convert screen recovery.
    fn migrate_v4(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS batch_state (
                id          INTEGER PRIMARY KEY CHECK (id = 1),
                paths_json  TEXT NOT NULL,
                format      TEXT,
                sample_rate INTEGER,
                bit_depth   TEXT,
                dither      TEXT,
                replaygain  TEXT,
                saved_at    TEXT NOT NULL
            );
        ",
            )
            .map_err(|e| format!("v4 migration failed: {}", e))?;
        Ok(())
    }

    // ── Batch state ──────────────────────────────────────────────

    /// Save the current Convert screen batch state for recovery.
    /// Uses id=1 (singleton row) — only one batch at a time.
    pub fn save_batch_state(
        &self,
        paths: &[std::path::PathBuf],
        format: Option<&str>,
        sample_rate: Option<u32>,
        bit_depth: Option<&str>,
        dither: Option<&str>,
        replaygain: Option<&str>,
    ) -> Result<(), String> {
        let paths_json = serde_json::to_string(
            &paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>(),
        )
        .map_err(|e| format!("paths serialize: {}", e))?;

        self.conn
            .execute(
                "INSERT OR REPLACE INTO batch_state (
                id, paths_json, format, sample_rate, bit_depth,
                dither, replaygain, saved_at
            ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    paths_json,
                    format,
                    sample_rate,
                    bit_depth,
                    dither,
                    replaygain,
                    chrono::Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|e| format!("batch state save: {}", e))?;
        Ok(())
    }

    /// Load the saved batch state, if any. Returns (paths, format, sample_rate,
    /// bit_depth, dither, replaygain).
    pub fn load_batch_state(
        &self,
    ) -> Option<(
        Vec<std::path::PathBuf>,
        Option<String>,
        Option<u32>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> {
        self.conn
            .query_row(
                "SELECT paths_json, format, sample_rate, bit_depth, dither, replaygain
             FROM batch_state WHERE id = 1",
                [],
                |row| {
                    let json: String = row.get(0)?;
                    let format: Option<String> = row.get(1)?;
                    let sample_rate: Option<u32> = row.get(2)?;
                    let bit_depth: Option<String> = row.get(3)?;
                    let dither: Option<String> = row.get(4)?;
                    let replaygain: Option<String> = row.get(5)?;
                    Ok((json, format, sample_rate, bit_depth, dither, replaygain))
                },
            )
            .ok()
            .and_then(|(json, format, sr, bd, dither, rg)| {
                let path_strs: Vec<String> = serde_json::from_str(&json).ok()?;
                let paths: Vec<std::path::PathBuf> = path_strs
                    .into_iter()
                    .map(std::path::PathBuf::from)
                    .filter(|p| p.exists()) // Only restore paths that still exist
                    .collect();
                if paths.is_empty() {
                    return None;
                }
                Some((paths, format, sr, bd, dither, rg))
            })
    }

    /// Clear the saved batch state (after commit or explicit cancel).
    pub fn clear_batch_state(&self) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM batch_state WHERE id = 1", [])
            .map_err(|e| format!("batch state clear: {}", e))?;
        Ok(())
    }

    /// v5: add access_count to recent_files.
    fn migrate_v5(conn: &Connection) -> Result<(), String> {
        if !Self::legacy_add_column_present(
            conn,
            "v5",
            "recent_files",
            "access_count",
            "INTEGER",
            true,
            Some("1"),
        )? {
            conn.execute_batch(
                "
            ALTER TABLE recent_files ADD COLUMN access_count INTEGER NOT NULL DEFAULT 1;
        ",
            )
            .map_err(|e| format!("v5 migration failed: {}", e))?;
        }
        Ok(())
    }

    /// v6: conversion queue table.
    fn migrate_v6(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS conversion_queue (
                id              TEXT PRIMARY KEY,
                item_json       TEXT NOT NULL
            );
        ",
            )
            .map_err(|e| format!("v6 migration failed: {}", e))?;
        Ok(())
    }

    /// v7: analysis cache table.
    fn migrate_v7(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS analysis_cache (
                file_path       TEXT PRIMARY KEY,
                file_mtime      INTEGER NOT NULL,
                file_size       INTEGER NOT NULL,
                dr_value        INTEGER,
                peak_db         REAL,
                rms_db          REAL,
                clipping_count  INTEGER,
                dc_bias         REAL,
                actual_bit_depth INTEGER,
                declared_bit_depth INTEGER,
                sample_rate     INTEGER,
                channels        INTEGER,
                duration_secs   REAL,
                lufs            REAL,
                true_peak_dbtp  REAL,
                analyzed_at     TEXT NOT NULL
            );
        ",
            )
            .map_err(|e| format!("v7 migration failed: {}", e))?;
        Ok(())
    }

    /// v8: drop + recreate analysis_cache with algo_version column.
    /// Invalidates all v7 cached results (algorithm was buggy).
    fn migrate_v8(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            DROP TABLE IF EXISTS analysis_cache;
            CREATE TABLE analysis_cache (
                file_path       TEXT PRIMARY KEY,
                file_mtime      INTEGER NOT NULL,
                file_size       INTEGER NOT NULL,
                algo_version    INTEGER NOT NULL,
                dr_value        INTEGER,
                peak_db         REAL,
                rms_db          REAL,
                clipping_count  INTEGER,
                dc_bias         REAL,
                actual_bit_depth INTEGER,
                declared_bit_depth INTEGER,
                sample_rate     INTEGER,
                channels        INTEGER,
                duration_secs   REAL,
                lufs            REAL,
                true_peak_dbtp  REAL,
                analyzed_at     TEXT NOT NULL
            );
        ",
            )
            .map_err(|e| format!("v8 migration failed: {}", e))?;
        Ok(())
    }

    /// v9: search tag cache table.
    fn migrate_v9(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS search_tag_cache (
                file_path       TEXT PRIMARY KEY,
                file_mtime      INTEGER NOT NULL,
                file_size       INTEGER NOT NULL,
                title           TEXT,
                artist          TEXT,
                album           TEXT,
                genre           TEXT,
                year            TEXT,
                tag_string      TEXT NOT NULL,
                last_accessed   TEXT NOT NULL
            );
        ",
            )
            .map_err(|e| format!("v9 migration failed: {}", e))?;
        Ok(())
    }

    /// v10: add preemphasis column to analysis_cache + bump algo version.
    fn migrate_v10(conn: &Connection) -> Result<(), String> {
        // Add columns independently because historical runs could commit the
        // first ALTER before failing on the second. Existing rows have NULL,
        // which is fine — the algo version bump means they won't be served
        // from cache anyway.
        if !Self::legacy_add_column_present(
            conn,
            "v10",
            "analysis_cache",
            "preemphasis",
            "INTEGER",
            false,
            None,
        )? {
            conn.execute_batch(
                "ALTER TABLE analysis_cache ADD COLUMN preemphasis INTEGER;",
            )
            .map_err(|e| format!("v10 migration failed: {}", e))?;
        }
        if !Self::legacy_add_column_present(
            conn,
            "v10",
            "analysis_cache",
            "preemphasis_corr",
            "REAL",
            false,
            None,
        )? {
            conn.execute_batch(
                "ALTER TABLE analysis_cache ADD COLUMN preemphasis_corr REAL;",
            )
            .map_err(|e| format!("v10 migration failed: {}", e))?;
        }
        Ok(())
    }

    /// v11: add preemph_corpus table for spectral scorer model storage.
    fn migrate_v11(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS preemph_corpus (
                id          INTEGER PRIMARY KEY DEFAULT 1,
                n_frames    INTEGER NOT NULL,
                n_tracks    INTEGER NOT NULL,
                mean        BLOB NOT NULL,
                covariance  BLOB NOT NULL,
                pca         BLOB NOT NULL,
                updated_at  TEXT NOT NULL
            );
        ",
            )
            .map_err(|e| format!("v11 migration failed: {}", e))?;
        Ok(())
    }

    /// v12: add empirical PE template column to preemph_corpus.
    fn migrate_v12(conn: &Connection) -> Result<(), String> {
        if !Self::legacy_add_column_present(
            conn,
            "v12",
            "preemph_corpus",
            "pe_template",
            "BLOB",
            false,
            None,
        )? {
            conn.execute_batch(
                "
            ALTER TABLE preemph_corpus ADD COLUMN pe_template BLOB;
        ",
            )
            .map_err(|e| format!("v12 migration failed: {}", e))?;
        }
        Ok(())
    }

    /// v13: add preemph_classifier table for trained LDA classifier storage.
    fn migrate_v13(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS preemph_classifier (
                id              INTEGER PRIMARY KEY DEFAULT 1,
                weights         BLOB NOT NULL,
                bias            REAL NOT NULL,
                threshold       REAL NOT NULL,
                feature_impute  BLOB NOT NULL,
                feature_means   BLOB NOT NULL,
                feature_stds    BLOB NOT NULL,
                cv_accuracy     REAL,
                cv_fpr          REAL,
                cv_precision    REAL,
                trained_at      TEXT NOT NULL
            );
        ",
            )
            .map_err(|e| format!("v13 migration failed: {}", e))?;
        Ok(())
    }

    /// v14: add preemphasis_detail column to analysis_cache.
    fn migrate_v14(conn: &Connection) -> Result<(), String> {
        if !Self::legacy_add_column_present(
            conn,
            "v14",
            "analysis_cache",
            "preemphasis_detail",
            "TEXT",
            false,
            None,
        )? {
            conn.execute_batch(
                "
            ALTER TABLE analysis_cache ADD COLUMN preemphasis_detail TEXT;
        ",
            )
            .map_err(|e| format!("v14 migration failed: {}", e))?;
        }
        Ok(())
    }

    /// v15: add HDCD columns to analysis_cache.
    fn migrate_v15(conn: &Connection) -> Result<(), String> {
        if !Self::legacy_add_column_present(
            conn,
            "v15",
            "analysis_cache",
            "hdcd_detected",
            "INTEGER",
            false,
            None,
        )? {
            conn.execute_batch(
                "ALTER TABLE analysis_cache ADD COLUMN hdcd_detected INTEGER;",
            )
            .map_err(|e| format!("v15 migration failed: {}", e))?;
        }
        if !Self::legacy_add_column_present(
            conn,
            "v15",
            "analysis_cache",
            "hdcd_detail",
            "TEXT",
            false,
            None,
        )? {
            conn.execute_batch(
                "ALTER TABLE analysis_cache ADD COLUMN hdcd_detail TEXT;",
            )
            .map_err(|e| format!("v15 migration failed: {}", e))?;
        }
        Ok(())
    }

    /// v16: AccurateRip result cache.
    fn migrate_v16(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS ar_cache (
                file_path TEXT NOT NULL,
                track_number INTEGER NOT NULL,
                file_mtime INTEGER NOT NULL,
                file_size INTEGER NOT NULL,
                disc_id TEXT NOT NULL,
                status TEXT NOT NULL,
                confidence INTEGER,
                ar_offset INTEGER,
                crc_v1 INTEGER NOT NULL,
                crc_v2 INTEGER NOT NULL,
                verified_at TEXT NOT NULL,
                PRIMARY KEY (file_path, track_number)
            );
        ",
            )
            .map_err(|e| format!("v16 migration failed: {}", e))?;
        Ok(())
    }

    /// v17: CTDB parity matrix cache (LRU by accessed_at).
    fn migrate_v17(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS ctdb_parity_cache (
                cache_key TEXT NOT NULL,
                npar INTEGER NOT NULL,
                stride INTEGER NOT NULL,
                parity_blob BLOB NOT NULL,
                cached_at TEXT NOT NULL,
                accessed_at TEXT NOT NULL,
                PRIMARY KEY (cache_key, npar)
            );
            CREATE INDEX IF NOT EXISTS idx_ctdb_parity_accessed
                ON ctdb_parity_cache (accessed_at);
        ",
            )
            .map_err(|e| format!("v17 migration failed: {}", e))?;
        Ok(())
    }

    fn migrate_v18(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS musicbrainz_toc_cache (
                toc_string TEXT PRIMARY KEY,
                response_json TEXT NOT NULL,
                fetched_at TEXT NOT NULL,
                accessed_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mb_accessed
                ON musicbrainz_toc_cache (accessed_at);
        ",
            )
            .map_err(|e| format!("v18 migration failed: {}", e))?;
        Ok(())
    }

    /// v19 (Phase B-5): MusicBrainz text-search + release-detail cache.
    /// Distinct from the TOC cache so the two namespaces evict independently
    /// — text search rows churn faster than TOC rows since the same disc
    /// can produce many search-query keys but only one TOC.
    fn migrate_v19(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS musicbrainz_search_cache (
                cache_key TEXT PRIMARY KEY,
                response_json TEXT NOT NULL,
                fetched_at TEXT NOT NULL,
                accessed_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mb_search_accessed
                ON musicbrainz_search_cache (accessed_at);
        ",
            )
            .map_err(|e| format!("v19 migration failed: {}", e))?;
        Ok(())
    }


    /// v20: crash recovery records for deferred archive-edit staging sessions.
    fn migrate_v20(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS pending_archive_sessions (
                archive_path        TEXT PRIMARY KEY,
                staging_dir         TEXT NOT NULL,
                archive_mtime_secs  INTEGER NOT NULL,
                archive_mtime_nanos INTEGER NOT NULL DEFAULT 0,
                archive_size        INTEGER NOT NULL,
                edits_json          TEXT NOT NULL,
                created_at          TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at          TEXT NOT NULL DEFAULT (datetime('now'))
            );
        ",
            )
            .map_err(|e| format!("v20 migration failed: {}", e))?;
        Ok(())
    }

    /// v21: identity-keyed Browse directory-summary cache.
    ///
    /// These rows cache only scoped directory summary facts. The persisted
    /// payload carries its own scope semantics: immediate and depth-2 facts are
    /// valid only for the focused directory identity, while recursive stats are
    /// explicitly best-effort because ancestor directory mtimes do not reliably
    /// reflect deep descendant edits on all filesystems.
    fn migrate_v21(conn: &Connection) -> Result<(), String> {
        conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS directory_summary_cache (
                dir_path             TEXT PRIMARY KEY,
                identity_size        INTEGER NOT NULL,
                identity_mtime_nanos INTEGER NOT NULL,
                strongest_scope      TEXT NOT NULL,
                payload              TEXT NOT NULL,
                cached_at            TEXT NOT NULL,
                accessed_at          TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_directory_summary_accessed
                ON directory_summary_cache (accessed_at);
        ",
            )
            .map_err(|e| format!("v21 migration failed: {}", e))?;
        Ok(())
    }

    pub fn upsert_pending_archive_session(
        &self,
        archive_path: &std::path::Path,
        staging_dir: &std::path::Path,
        archive_mtime_secs: i64,
        archive_mtime_nanos: u32,
        archive_size: u64,
        edits_json: &str,
    ) -> Result<(), String> {
        self.conn.execute(
            "INSERT INTO pending_archive_sessions (
                archive_path, staging_dir, archive_mtime_secs, archive_mtime_nanos,
                archive_size, edits_json, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), datetime('now'))
             ON CONFLICT(archive_path) DO UPDATE SET
                staging_dir=excluded.staging_dir,
                archive_mtime_secs=excluded.archive_mtime_secs,
                archive_mtime_nanos=excluded.archive_mtime_nanos,
                archive_size=excluded.archive_size,
                edits_json=excluded.edits_json,
                updated_at=datetime('now')",
            params![
                archive_path.display().to_string(),
                staging_dir.display().to_string(),
                archive_mtime_secs,
                i64::from(archive_mtime_nanos),
                archive_size as i64,
                edits_json,
            ],
        ).map_err(|e| format!("pending archive session save: {e}"))?;
        Ok(())
    }

    pub fn delete_pending_archive_session(&self, archive_path: &std::path::Path) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM pending_archive_sessions WHERE archive_path = ?1",
                params![archive_path.display().to_string()],
            )
            .map_err(|e| format!("pending archive session delete: {e}"))?;
        Ok(())
    }

    pub fn recover_pending_archive_sessions_at_startup(&self) -> Result<Vec<PendingArchiveSessionRecovery>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT archive_path, staging_dir, archive_mtime_secs, archive_mtime_nanos, archive_size, edits_json
             FROM pending_archive_sessions
             ORDER BY updated_at DESC"
        ).map_err(|e| format!("pending archive session query: {e}"))?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        }).map_err(|e| format!("pending archive session scan: {e}"))?;

        let mut sessions = Vec::new();
        for row in rows {
            let (archive_path, staging_dir, mtime_secs, mtime_nanos, archive_size, edits_json) =
                row.map_err(|e| format!("pending archive session row: {e}"))?;
            let archive = std::path::PathBuf::from(&archive_path);
            let staging = std::path::PathBuf::from(&staging_dir);

            if looks_like_test_archive_session_artifact(&archive, &staging) {
                let _ = std::fs::remove_dir_all(&staging);
                let _ = self.delete_pending_archive_session(&archive);
                continue;
            }

            if !staging.is_dir() {
                let _ = self.delete_pending_archive_session(&archive);
                continue;
            }

            let mut conflicted = false;
            let mut conflict_reason = None;
            match std::fs::metadata(&archive) {
                Ok(meta) => {
                    match meta.modified().ok().and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok()) {
                        Some(modified) => {
                            if modified.as_secs() as i64 != mtime_secs
                                || i64::from(modified.subsec_nanos()) != mtime_nanos
                                || meta.len() as i64 != archive_size
                            {
                                conflicted = true;
                                conflict_reason = Some("archive file changed since staging was created".to_string());
                            }
                        }
                        None => {
                            conflicted = true;
                            conflict_reason = Some("archive modification time could not be read".to_string());
                        }
                    }
                }
                Err(err) => {
                    conflicted = true;
                    conflict_reason = Some(format!("archive file is missing or unreadable: {err}"));
                }
            }

            sessions.push(PendingArchiveSessionRecovery {
                archive_path: archive,
                staging_dir: staging,
                archive_mtime_secs: mtime_secs,
                archive_mtime_nanos: u32::try_from(mtime_nanos).unwrap_or_default(),
                archive_size: u64::try_from(archive_size).unwrap_or_default(),
                edits_json,
                conflicted,
                conflict_reason,
            });
        }

        Ok(sessions)
    }

    pub fn reconcile_pending_archive_sessions_at_startup(&self) -> Result<(usize, usize), String> {
        let sessions = self.recover_pending_archive_sessions_at_startup()?;
        let valid = sessions.iter().filter(|session| !session.conflicted).count();
        let conflicted = sessions.len().saturating_sub(valid);
        Ok((valid, conflicted))
    }

    #[cfg(test)]
    pub fn pending_archive_session_count_for_tests(&self) -> Result<usize, String> {
        self.conn
            .query_row("SELECT COUNT(*) FROM pending_archive_sessions", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count as usize)
            .map_err(|e| format!("pending archive session count: {e}"))
    }

    // ── AccurateRip cache ───────────────────────────────────────


    /// v22: distinguish in-flight writes from committed and rolled-back
    /// cleanup states. This prevents startup recovery from restoring an old
    /// backup over a write that committed before cleanup failed.
    fn migrate_v22(conn: &Connection) -> Result<(), String> {
        let mut columns = conn
            .prepare("PRAGMA table_info(metadata_journal)")
            .map_err(|error| format!("v22 migration inspect metadata_journal: {error}"))?;
        let names = columns
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| format!("v22 migration inspect metadata_journal: {error}"))?;
        let mut has_state = false;
        for name in names {
            if name
                .map_err(|error| format!("v22 migration decode metadata_journal column: {error}"))?
                == "state"
            {
                has_state = true;
                break;
            }
        }
        drop(columns);

        if !has_state {
            conn
                .execute_batch(
                    "ALTER TABLE metadata_journal ADD COLUMN state TEXT NOT NULL DEFAULT 'prepared';",
                )
                .map_err(|error| format!("v22 migration: {error}"))?;
        }
        Ok(())
    }

    /// v23: make SQLite the explicit queue/recent-files authority.
    ///
    /// The queue gains a durable ordinal, while the singleton import-state row
    /// records whether each legacy JSON store has already been retired as a
    /// startup authority. Existing dual-write users are initialized as done
    /// whenever the corresponding SQLite table already contains rows.
    fn migrate_v23(conn: &Connection) -> Result<(), String> {
        if !Self::legacy_add_column_present(
            conn,
            "v23",
            "conversion_queue",
            "position",
            "INTEGER",
            true,
            Some("0"),
        )? {
            conn.execute_batch(
                "ALTER TABLE conversion_queue ADD COLUMN position INTEGER NOT NULL DEFAULT 0;",
            )
            .map_err(|error| format!("v23 migration add conversion_queue.position: {error}"))?;
        }

        // Rowid is the best ordering signal available in the pre-v23 schema:
        // the historical full rewrite inserted rows in queue order. Re-number
        // every row inside this migration transaction so an idempotent retry
        // cannot leave duplicate default-zero ordinals behind.
        let mut rows = conn
            .prepare("SELECT rowid, id FROM conversion_queue ORDER BY rowid")
            .map_err(|error| format!("v23 migration read queue row order: {error}"))?;
        let queue_rows = rows
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
            .map_err(|error| format!("v23 migration query queue row order: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("v23 migration decode queue row order: {error}"))?;
        drop(rows);
        for (position, (rowid, _id)) in queue_rows.iter().enumerate() {
            conn.execute(
                "UPDATE conversion_queue SET position = ?1 WHERE rowid = ?2",
                params![position as i64, rowid],
            )
            .map_err(|error| format!("v23 migration assign queue position: {error}"))?;
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS legacy_json_import_state (
                id                  INTEGER PRIMARY KEY CHECK (id = 1),
                queue_import_done   INTEGER NOT NULL CHECK (queue_import_done IN (0, 1)),
                recent_import_done  INTEGER NOT NULL CHECK (recent_import_done IN (0, 1))
            );",
        )
        .map_err(|error| format!("v23 migration create legacy import state: {error}"))?;

        // Existing dual-write builds could already have accumulated an
        // unbounded SQLite recent-files table. Enforce the product retention
        // bound during the upgrade itself; subsequent record/update calls keep
        // the bound transactionally.
        Self::prune_recent_rows(conn, RECENT_FILES_RETENTION_LIMIT)
            .map_err(|error| format!("v23 migration {error}"))?;

        conn.execute(
            "INSERT OR IGNORE INTO legacy_json_import_state (
                id, queue_import_done, recent_import_done
             ) VALUES (
                ?1,
                CASE WHEN EXISTS (SELECT 1 FROM conversion_queue LIMIT 1) THEN 1 ELSE 0 END,
                CASE WHEN EXISTS (SELECT 1 FROM recent_files LIMIT 1) THEN 1 ELSE 0 END
             )",
            [LEGACY_IMPORT_STATE_ROW_ID],
        )
        .map_err(|error| format!("v23 migration initialize legacy import state: {error}"))?;

        Ok(())
    }

    /// Look up cached AR results for a file. Returns None if not cached
    /// or stale (mtime/size changed).
    pub fn get_cached_ar(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
    ) -> Option<Vec<crate::tui::accuraterip::ArTrackResult>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT track_number, status, confidence, ar_offset, crc_v1, crc_v2
             FROM ar_cache
             WHERE file_path = ?1 AND file_mtime = ?2 AND file_size = ?3
             ORDER BY track_number",
            )
            .ok()?;

        let results: Vec<crate::tui::accuraterip::ArTrackResult> = stmt
            .query_map(params![file_path, mtime, size as i64], |row| {
                let track_number: u32 = row.get(0)?;
                let status_str: String = row.get(1)?;
                let confidence: Option<u8> = row.get(2)?;
                let ar_offset: Option<i32> = row.get(3)?;
                let crc_v1: u32 = row.get::<_, i64>(4)? as u32;
                let crc_v2: u32 = row.get::<_, i64>(5)? as u32;

                let status = match status_str.as_str() {
                    "verified" => crate::tui::accuraterip::ArTrackStatus::Verified,
                    "mismatch" => crate::tui::accuraterip::ArTrackStatus::Mismatch,
                    "not_in_db" => crate::tui::accuraterip::ArTrackStatus::NoDiscInDatabase,
                    _ => crate::tui::accuraterip::ArTrackStatus::Error(status_str),
                };

                Ok(crate::tui::accuraterip::ArTrackResult {
                    path: std::path::PathBuf::from(file_path),
                    track_number,
                    status,
                    confidence,
                    offset: ar_offset,
                    crc_v1,
                    crc_v2,
                })
            })
            .ok()?
            .filter_map(|r| r.ok())
            .collect();

        if results.is_empty() {
            None
        } else {
            Some(results)
        }
    }

    /// Store AR verification results in the cache.
    pub fn store_ar(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
        results: &[crate::tui::accuraterip::ArTrackResult],
        disc_id: &str,
    ) -> Result<(), String> {
        // Delete old entries for this file (might have different track count).
        self.conn
            .execute(
                "DELETE FROM ar_cache WHERE file_path = ?1",
                params![file_path],
            )
            .map_err(|e| format!("ar cache delete: {}", e))?;

        let now = chrono::Utc::now().to_rfc3339();
        for t in results {
            let status_str = match &t.status {
                crate::tui::accuraterip::ArTrackStatus::Verified => "verified",
                crate::tui::accuraterip::ArTrackStatus::Mismatch => "mismatch",
                crate::tui::accuraterip::ArTrackStatus::NoDiscInDatabase => "not_in_db",
                crate::tui::accuraterip::ArTrackStatus::Error(_) => "error",
            };
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO ar_cache (
                    file_path, track_number, file_mtime, file_size,
                    disc_id, status, confidence, ar_offset, crc_v1, crc_v2, verified_at
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        file_path,
                        t.track_number,
                        mtime,
                        size as i64,
                        disc_id,
                        status_str,
                        t.confidence,
                        t.offset,
                        t.crc_v1 as i64,
                        t.crc_v2 as i64,
                        now,
                    ],
                )
                .map_err(|e| format!("ar cache store: {}", e))?;
        }
        Ok(())
    }

    // ── CTDB parity matrix cache ────────────────────────────────

    /// Look up the cached CTDB parity matrix for a disc, keyed by a
    /// content hash of the audio inputs. Returns the deserialized matrix
    /// on hit and updates the row's `accessed_at` so LRU eviction reflects
    /// recent use. Returns `None` on cache miss or any decode error.
    pub fn get_cached_ctdb_parity(&self, cache_key: &str, npar: u32) -> Option<Vec<Vec<u16>>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT stride, parity_blob FROM ctdb_parity_cache
             WHERE cache_key = ?1 AND npar = ?2",
            )
            .ok()?;

        let row: Option<(i64, Vec<u8>)> = stmt
            .query_row(params![cache_key, npar as i64], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .ok();

        let (stride_i64, blob) = row?;
        let stride = stride_i64 as usize;
        let parity =
            crate::ctdb_rs::syndrome::try_bytes_to_parity(&blob, stride, npar as usize).ok()?;

        // Touch accessed_at so LRU treats this as recent. Failures here are
        // non-fatal — the cache hit is still valid; we just missed a bookkeeping
        // update.
        let now = chrono::Utc::now().to_rfc3339();
        let _ = self.conn.execute(
            "UPDATE ctdb_parity_cache SET accessed_at = ?1
             WHERE cache_key = ?2 AND npar = ?3",
            params![now, cache_key, npar as i64],
        );

        Some(parity)
    }

    /// Store a parity matrix in the cache. Triggers LRU eviction if the
    /// row count exceeds `CTDB_PARITY_CACHE_EVICT_THRESHOLD`, trimming
    /// down to `CTDB_PARITY_CACHE_EVICT_TARGET` by evicting the least
    /// recently accessed rows.
    pub fn store_ctdb_parity(
        &self,
        cache_key: &str,
        npar: u32,
        parity: &[Vec<u16>],
    ) -> Result<(), String> {
        if parity.is_empty() {
            return Err("empty parity matrix".to_string());
        }
        let stride = parity.len();
        let blob = crate::ctdb_rs::syndrome::parity_to_bytes(parity, stride, npar as usize);

        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT OR REPLACE INTO ctdb_parity_cache
                 (cache_key, npar, stride, parity_blob, cached_at, accessed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![cache_key, npar as i64, stride as i64, blob, now],
            )
            .map_err(|e| format!("ctdb parity cache store: {}", e))?;

        // Eviction: only deletes when count exceeds the threshold, then
        // trims down to the target. Cheap on the common case (single
        // SELECT COUNT).
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM ctdb_parity_cache", [], |row| {
                row.get(0)
            })
            .map_err(|e| format!("ctdb parity cache count: {}", e))?;

        if (count as usize) > CTDB_PARITY_CACHE_EVICT_THRESHOLD {
            let to_remove = (count as usize) - CTDB_PARITY_CACHE_EVICT_TARGET;
            self.conn
                .execute(
                    "DELETE FROM ctdb_parity_cache
                 WHERE rowid IN (
                     SELECT rowid FROM ctdb_parity_cache
                     ORDER BY accessed_at ASC
                     LIMIT ?1
                 )",
                    params![to_remove as i64],
                )
                .map_err(|e| format!("ctdb parity cache evict: {}", e))?;
        }

        Ok(())
    }

    // ── MusicBrainz TOC cache ────────────────────────────────────

    /// Look up a cached MusicBrainz response by TOC string. Returns the raw
    /// JSON body on hit. Returns `None` on miss or when the entry is older
    /// than the 30-day TTL. Touches `accessed_at` on hit (LRU).
    pub fn get_cached_mb_response(&self, toc_string: &str) -> Option<String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT response_json, fetched_at FROM musicbrainz_toc_cache
             WHERE toc_string = ?1",
            )
            .ok()?;

        let row: Option<(String, String)> = stmt
            .query_row(params![toc_string], |row| Ok((row.get(0)?, row.get(1)?)))
            .ok();
        let (json, fetched_at) = row?;

        // TTL check: parse RFC3339, drop entry if older than 30 days.
        if let Ok(fetched) = chrono::DateTime::parse_from_rfc3339(&fetched_at) {
            let age = chrono::Utc::now().signed_duration_since(fetched.with_timezone(&chrono::Utc));
            if age.num_seconds() > MB_CACHE_TTL_SECS {
                return None;
            }
        } else {
            return None;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let _ = self.conn.execute(
            "UPDATE musicbrainz_toc_cache SET accessed_at = ?1 WHERE toc_string = ?2",
            params![now, toc_string],
        );
        Some(json)
    }

    /// Store a MusicBrainz response. Triggers LRU eviction when the row
    /// count exceeds `MB_CACHE_EVICT_THRESHOLD`, trimming to
    /// `MB_CACHE_EVICT_TARGET`.
    pub fn store_mb_response(&self, toc_string: &str, response_json: &str) -> Result<(), String> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT OR REPLACE INTO musicbrainz_toc_cache
                 (toc_string, response_json, fetched_at, accessed_at)
             VALUES (?1, ?2, ?3, ?3)",
                params![toc_string, response_json, now],
            )
            .map_err(|e| format!("mb cache store: {}", e))?;

        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM musicbrainz_toc_cache", [], |row| {
                row.get(0)
            })
            .map_err(|e| format!("mb cache count: {}", e))?;

        if (count as usize) > MB_CACHE_EVICT_THRESHOLD {
            let to_remove = (count as usize) - MB_CACHE_EVICT_TARGET;
            self.conn
                .execute(
                    "DELETE FROM musicbrainz_toc_cache
                 WHERE rowid IN (
                     SELECT rowid FROM musicbrainz_toc_cache
                     ORDER BY accessed_at ASC
                     LIMIT ?1
                 )",
                    params![to_remove as i64],
                )
                .map_err(|e| format!("mb cache evict: {}", e))?;
        }
        Ok(())
    }

    // ── MusicBrainz search + release-detail cache (Phase B-5) ────

    /// Look up a cached MusicBrainz body by canonical query key. Returns
    /// the raw JSON on hit. Returns `None` on miss or when the entry is
    /// older than the 30-day TTL (shared with the TOC cache). Touches
    /// `accessed_at` on hit (LRU).
    ///
    /// Keys are produced by `musicbrainz::search_cache_key` (text search)
    /// or `musicbrainz::detail_cache_key` (release detail); both share
    /// this table to keep eviction simple.
    pub fn get_cached_mb_search(&self, cache_key: &str) -> Option<String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT response_json, fetched_at FROM musicbrainz_search_cache
             WHERE cache_key = ?1",
            )
            .ok()?;

        let row: Option<(String, String)> = stmt
            .query_row(params![cache_key], |row| Ok((row.get(0)?, row.get(1)?)))
            .ok();
        let (json, fetched_at) = row?;

        // TTL check shared with TOC cache (30 days). Drop stale rows.
        if let Ok(fetched) = chrono::DateTime::parse_from_rfc3339(&fetched_at) {
            let age = chrono::Utc::now().signed_duration_since(fetched.with_timezone(&chrono::Utc));
            if age.num_seconds() > MB_CACHE_TTL_SECS {
                return None;
            }
        } else {
            return None;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let _ = self.conn.execute(
            "UPDATE musicbrainz_search_cache SET accessed_at = ?1 WHERE cache_key = ?2",
            params![now, cache_key],
        );
        Some(json)
    }

    /// Store a MusicBrainz response under the canonical query key.
    /// Triggers LRU eviction when row count exceeds the threshold.
    pub fn store_mb_search(&self, cache_key: &str, response_json: &str) -> Result<(), String> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT OR REPLACE INTO musicbrainz_search_cache
                 (cache_key, response_json, fetched_at, accessed_at)
             VALUES (?1, ?2, ?3, ?3)",
                params![cache_key, response_json, now],
            )
            .map_err(|e| format!("mb search cache store: {}", e))?;

        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM musicbrainz_search_cache", [], |row| {
                row.get(0)
            })
            .map_err(|e| format!("mb search cache count: {}", e))?;

        if (count as usize) > MB_SEARCH_CACHE_EVICT_THRESHOLD {
            let to_remove = (count as usize) - MB_SEARCH_CACHE_EVICT_TARGET;
            self.conn
                .execute(
                    "DELETE FROM musicbrainz_search_cache
                 WHERE rowid IN (
                     SELECT rowid FROM musicbrainz_search_cache
                     ORDER BY accessed_at ASC
                     LIMIT ?1
                 )",
                    params![to_remove as i64],
                )
                .map_err(|e| format!("mb search cache evict: {}", e))?;
        }
        Ok(())
    }

    // ── Search tag cache ─────────────────────────────────────────

    /// Look up cached tag string. Returns (tag_string, title, artist, album, genre, year)
    /// on hit. Updates last_accessed. Returns None if not cached or stale.
    pub fn get_cached_tags(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
    ) -> Option<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> {
        let row = self
            .conn
            .query_row(
                "SELECT tag_string, title, artist, album, genre, year
             FROM search_tag_cache
             WHERE file_path = ?1 AND file_mtime = ?2 AND file_size = ?3",
                params![file_path, mtime, size as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .ok()?;

        // Update last_accessed.
        let _ = self.conn.execute(
            "UPDATE search_tag_cache SET last_accessed = ?1 WHERE file_path = ?2",
            params![chrono::Utc::now().to_rfc3339(), file_path],
        );

        Some(row)
    }

    /// Store a tag string in the search cache.
    pub fn store_cached_tags(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
        title: Option<&str>,
        artist: Option<&str>,
        album: Option<&str>,
        genre: Option<&str>,
        year: Option<&str>,
        tag_string: &str,
    ) -> Result<(), String> {
        self.conn.execute(
            "INSERT OR REPLACE INTO search_tag_cache
             (file_path, file_mtime, file_size, title, artist, album, genre, year, tag_string, last_accessed)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                file_path, mtime, size as i64,
                title, artist, album, genre, year,
                tag_string,
                chrono::Utc::now().to_rfc3339(),
            ],
        ).map_err(|e| format!("search tag cache store: {}", e))?;
        Ok(())
    }

    /// Prune search tag cache entries not accessed in the last `days` days.
    pub fn prune_search_tag_cache(&self, days: u32) {
        let _ = self.conn.execute(
            &format!(
                "DELETE FROM search_tag_cache WHERE last_accessed < datetime('now', '-{} days')",
                days
            ),
            [],
        );
    }

    // ── Analysis cache ───────────────────────────────────────────

    /// Bump this when the analysis algorithm changes to invalidate
    /// cached results computed by an older version.
    const ANALYSIS_ALGO_VERSION: i32 = 25;

    /// Look up cached analysis. Returns None if not cached, stale,
    /// or computed by an older algorithm version.
    pub fn get_cached_analysis(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
    ) -> Option<crate::tui::analyze::AnalysisResult> {
        self.conn
            .query_row(
                "SELECT dr_value, peak_db, rms_db, clipping_count, dc_bias,
                    actual_bit_depth, declared_bit_depth, sample_rate, channels,
                    duration_secs, lufs, true_peak_dbtp, preemphasis, preemphasis_corr,
                    preemphasis_detail, hdcd_detected, hdcd_detail
             FROM analysis_cache
             WHERE file_path = ?1 AND file_mtime = ?2 AND file_size = ?3
               AND algo_version = ?4",
                params![file_path, mtime, size as i64, Self::ANALYSIS_ALGO_VERSION],
                |row| {
                    let preemph_int: Option<i32> = row.get(12)?;
                    let preemphasis = match preemph_int {
                        Some(3) => {
                            Some(crate::tui::preemphasis::PreemphasisConfidence::StrongCandidate)
                        }
                        Some(2) => Some(crate::tui::preemphasis::PreemphasisConfidence::Detected),
                        Some(1) => Some(crate::tui::preemphasis::PreemphasisConfidence::Possible),
                        Some(0) => Some(crate::tui::preemphasis::PreemphasisConfidence::NotDetected),
                        Some(-1) => Some(crate::tui::preemphasis::PreemphasisConfidence::Indeterminate),
                        _ => None,
                    };
                    let hdcd_int: Option<i32> = row.get(15)?;
                    Ok(crate::tui::analyze::AnalysisResult {
                        path: std::path::PathBuf::from(file_path),
                        dr_value: row.get(0)?,
                        peak_db: row.get(1)?,
                        rms_db: row.get(2)?,
                        clipping_count: row.get::<_, i64>(3)? as u64,
                        dc_bias: row.get(4)?,
                        actual_bit_depth: row.get(5)?,
                        declared_bit_depth: row.get(6)?,
                        sample_rate: row.get(7)?,
                        channels: row.get(8)?,
                        duration_secs: row.get(9)?,
                        lufs: row.get(10)?,
                        true_peak_dbtp: row.get(11)?,
                        preemphasis,
                        preemphasis_corr: row.get(13)?,
                        preemphasis_detail: row.get(14)?,
                        hdcd_detected: hdcd_int.map(|v| v != 0),
                        hdcd_detail: row.get(16)?,
                    })
                },
            )
            .ok()
    }

    /// Look up only the HDCD / Phase-2-safe pre-emphasis facts from the
    /// analysis cache. Unlike `get_cached_analysis`, this intentionally accepts
    /// rows that contain only narrow Details-tab facts and no DR/peak/RMS data.
    pub fn get_cached_metadata_analysis_facts(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
    ) -> Option<crate::tui::app::MetadataAnalysisFacts> {
        self.conn
            .query_row(
                "SELECT preemphasis, preemphasis_detail, hdcd_detected, hdcd_detail
                 FROM analysis_cache
                 WHERE file_path = ?1 AND file_mtime = ?2 AND file_size = ?3
                   AND algo_version = ?4",
                params![file_path, mtime, size as i64, Self::ANALYSIS_ALGO_VERSION],
                |row| {
                    let preemph_int: Option<i32> = row.get(0)?;
                    let preemphasis = match preemph_int {
                        Some(3) => {
                            Some(crate::tui::preemphasis::PreemphasisConfidence::StrongCandidate)
                        }
                        Some(2) => Some(crate::tui::preemphasis::PreemphasisConfidence::Detected),
                        Some(1) => Some(crate::tui::preemphasis::PreemphasisConfidence::Possible),
                        Some(0) => Some(crate::tui::preemphasis::PreemphasisConfidence::NotDetected),
                        Some(-1) => Some(crate::tui::preemphasis::PreemphasisConfidence::Indeterminate),
                        _ => None,
                    };
                    let hdcd_int: Option<i32> = row.get(2)?;
                    let facts = crate::tui::app::MetadataAnalysisFacts {
                        preemphasis,
                        preemphasis_detail: row.get(1)?,
                        hdcd_detected: hdcd_int.map(|value| value != 0),
                        hdcd_detail: row.get(3)?,
                    };
                    if facts.has_any_result() {
                        Ok(Some(facts))
                    } else {
                        Ok(None)
                    }
                },
            )
            .ok()
            .flatten()
    }

    /// Store only HDCD / Phase-2-safe pre-emphasis facts. Existing full analysis
    /// metrics are preserved when the file identity is unchanged, but are
    /// cleared if the same path now points at different bytes. This prevents
    /// the narrow Details analyzer from poisoning the full DR/peak/RMS cache.
    pub fn store_metadata_analysis_facts(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
        facts: &crate::tui::app::MetadataAnalysisFacts,
    ) -> Result<(), String> {
        if !facts.has_any_result() {
            return Ok(());
        }

        let preemphasis = facts.preemphasis.as_ref().map(|p| match p {
            crate::tui::preemphasis::PreemphasisConfidence::Detected => 2i32,
            crate::tui::preemphasis::PreemphasisConfidence::StrongCandidate => 3i32,
            crate::tui::preemphasis::PreemphasisConfidence::Possible => 1i32,
            crate::tui::preemphasis::PreemphasisConfidence::NotDetected => 0i32,
            crate::tui::preemphasis::PreemphasisConfidence::Indeterminate => -1i32,
        });
        let hdcd_detected = facts.hdcd_detected.map(|value| if value { 1i32 } else { 0i32 });

        self.conn.execute(
            "INSERT INTO analysis_cache (
                file_path, file_mtime, file_size, algo_version,
                preemphasis, preemphasis_detail, hdcd_detected, hdcd_detail, analyzed_at
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
            ON CONFLICT(file_path) DO UPDATE SET
                dr_value = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                  AND analysis_cache.file_size = excluded.file_size
                                THEN analysis_cache.dr_value ELSE NULL END,
                peak_db = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                 AND analysis_cache.file_size = excluded.file_size
                               THEN analysis_cache.peak_db ELSE NULL END,
                rms_db = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                AND analysis_cache.file_size = excluded.file_size
                              THEN analysis_cache.rms_db ELSE NULL END,
                clipping_count = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                        AND analysis_cache.file_size = excluded.file_size
                                      THEN analysis_cache.clipping_count ELSE NULL END,
                dc_bias = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                 AND analysis_cache.file_size = excluded.file_size
                               THEN analysis_cache.dc_bias ELSE NULL END,
                actual_bit_depth = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                          AND analysis_cache.file_size = excluded.file_size
                                        THEN analysis_cache.actual_bit_depth ELSE NULL END,
                declared_bit_depth = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                            AND analysis_cache.file_size = excluded.file_size
                                          THEN analysis_cache.declared_bit_depth ELSE NULL END,
                sample_rate = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                     AND analysis_cache.file_size = excluded.file_size
                                   THEN analysis_cache.sample_rate ELSE NULL END,
                channels = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                  AND analysis_cache.file_size = excluded.file_size
                                THEN analysis_cache.channels ELSE NULL END,
                duration_secs = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                       AND analysis_cache.file_size = excluded.file_size
                                     THEN analysis_cache.duration_secs ELSE NULL END,
                lufs = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                              AND analysis_cache.file_size = excluded.file_size
                            THEN analysis_cache.lufs ELSE NULL END,
                true_peak_dbtp = CASE WHEN analysis_cache.file_mtime = excluded.file_mtime
                                        AND analysis_cache.file_size = excluded.file_size
                                      THEN analysis_cache.true_peak_dbtp ELSE NULL END,
                file_mtime = excluded.file_mtime,
                file_size = excluded.file_size,
                algo_version = excluded.algo_version,
                preemphasis = CASE
                    WHEN analysis_cache.file_mtime = excluded.file_mtime
                     AND analysis_cache.file_size = excluded.file_size
                     AND excluded.preemphasis IS NULL
                    THEN analysis_cache.preemphasis
                    ELSE excluded.preemphasis
                END,
                preemphasis_detail = CASE
                    WHEN analysis_cache.file_mtime = excluded.file_mtime
                     AND analysis_cache.file_size = excluded.file_size
                     AND excluded.preemphasis IS NULL
                    THEN analysis_cache.preemphasis_detail
                    ELSE excluded.preemphasis_detail
                END,
                hdcd_detected = CASE
                    WHEN analysis_cache.file_mtime = excluded.file_mtime
                     AND analysis_cache.file_size = excluded.file_size
                     AND excluded.hdcd_detected IS NULL
                    THEN analysis_cache.hdcd_detected
                    ELSE excluded.hdcd_detected
                END,
                hdcd_detail = CASE
                    WHEN analysis_cache.file_mtime = excluded.file_mtime
                     AND analysis_cache.file_size = excluded.file_size
                     AND excluded.hdcd_detected IS NULL
                    THEN analysis_cache.hdcd_detail
                    ELSE excluded.hdcd_detail
                END,
                analyzed_at = excluded.analyzed_at",
            params![
                file_path,
                mtime,
                size as i64,
                Self::ANALYSIS_ALGO_VERSION,
                preemphasis,
                facts.preemphasis_detail.as_deref(),
                hdcd_detected,
                facts.hdcd_detail.as_deref(),
                chrono::Utc::now().to_rfc3339(),
            ],
        ).map_err(|e| format!("analysis facts cache store: {}", e))?;
        Ok(())
    }

    /// Store an analysis result in the cache.
    pub fn store_analysis(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
        r: &crate::tui::analyze::AnalysisResult,
    ) -> Result<(), String> {
        self.conn.execute(
            "INSERT OR REPLACE INTO analysis_cache (
                file_path, file_mtime, file_size, algo_version,
                dr_value, peak_db, rms_db, clipping_count, dc_bias,
                actual_bit_depth, declared_bit_depth, sample_rate, channels,
                duration_secs, lufs, true_peak_dbtp, preemphasis, preemphasis_corr,
                preemphasis_detail, hdcd_detected, hdcd_detail, analyzed_at
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                file_path, mtime, size as i64, Self::ANALYSIS_ALGO_VERSION,
                r.dr_value, r.peak_db, r.rms_db, r.clipping_count as i64, r.dc_bias,
                r.actual_bit_depth, r.declared_bit_depth, r.sample_rate, r.channels,
                r.duration_secs, r.lufs, r.true_peak_dbtp,
                r.preemphasis.as_ref().map(|p| match p {
                    crate::tui::preemphasis::PreemphasisConfidence::Detected => 2i32,
                    crate::tui::preemphasis::PreemphasisConfidence::StrongCandidate => 3i32,
                    crate::tui::preemphasis::PreemphasisConfidence::Possible => 1i32,
                    crate::tui::preemphasis::PreemphasisConfidence::NotDetected => 0i32,
                    crate::tui::preemphasis::PreemphasisConfidence::Indeterminate => -1i32,
                }),
                r.preemphasis_corr,
                r.preemphasis_detail.as_deref(),
                r.hdcd_detected.map(|b| if b { 1i32 } else { 0 }),
                r.hdcd_detail.as_deref(),
                chrono::Utc::now().to_rfc3339(),
            ],
        ).map_err(|e| format!("analysis cache store: {}", e))?;
        Ok(())
    }

    // ── Conversion queue ─────────────────────────────────────────

    fn prepare_queue_items_for_persistence(
        items: &[&crate::convert::ConversionItem],
    ) -> Result<
        (
            Vec<crate::convert::ConversionItem>,
            crate::convert::queue::QueueSecretPersistReport,
        ),
        String,
    > {
        let mut persisted_items = items
            .iter()
            .filter(|item| {
                !crate::convert::queue_expansion::is_synthetic_cue_album_artifact(
                    &item.input_path,
                )
            })
            .map(|item| (*item).clone())
            .collect::<Vec<_>>();

        for item in &mut persisted_items {
            if matches!(item.status, crate::convert::ConversionStatus::Processing { .. }) {
                item.status = crate::convert::ConversionStatus::Interrupted;
                item.started_at = None;
                item.completed_at = None;
            } else if matches!(item.status, crate::convert::ConversionStatus::Interrupted) {
                // Interrupted work is a durable intent state, never a resumable
                // worker checkpoint. Keep timestamps/progress from implying a
                // mid-tool invocation can continue after restart.
                item.started_at = None;
                item.completed_at = None;
            }
        }

        let persist_report =
            crate::convert::queue::prepare_archive_passwords_for_persistence(
                &mut persisted_items,
            )?;
        Ok((persisted_items, persist_report))
    }

    fn persisted_queue_reference(item_json: &str) -> Option<String> {
        // Extract the top-level reference even when the rest of a historical
        // row is no longer decodable as `ConversionItem`. This lets salvage
        // deletes retire a queue-owned credential after commit instead of
        // leaking it merely because an unrelated field is malformed.
        serde_json::from_str::<serde_json::Value>(item_json)
            .ok()
            .and_then(|value| {
                value
                    .get("archive_password_ref")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|reference| {
                crate::secret_store::reference_has_namespace(reference, "queue-item")
            })
    }

    fn dedup_unreferenced_queue_secret_refs(
        references: &mut Vec<String>,
        persisted_items: &[crate::convert::ConversionItem],
    ) {
        let live = persisted_items
            .iter()
            .filter_map(|item| item.archive_password_ref.as_ref())
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        references.retain(|reference| !live.contains(reference));
        references.sort();
        references.dedup();
    }

    fn durable_queue_secret_refs(
        persisted_items: &[crate::convert::ConversionItem],
    ) -> Vec<String> {
        let mut references = persisted_items
            .iter()
            .filter_map(|item| item.archive_password_ref.as_ref())
            .filter(|reference| {
                crate::secret_store::reference_has_namespace(reference, "queue-item")
            })
            .cloned()
            .collect::<Vec<_>>();
        references.sort();
        references.dedup();
        references
    }

    /// Incrementally reconcile SQLite with the current durable queue intent.
    ///
    /// The method serializes the desired snapshot to detect changes, but only
    /// writes rows whose JSON or ordinal changed and only deletes rows that are
    /// no longer present. This keeps terminal updates O(changed rows) at the
    /// SQLite write layer while preserving one transaction for row/order
    /// consistency. Queue-owned references stripped from successful terminal
    /// rows are retired only after commit. References belonging to
    /// deleted/superseded rows are returned to the caller because a live
    /// worker may still own one.
    pub fn sync_queue(
        &self,
        items: &[&crate::convert::ConversionItem],
    ) -> Result<QueueSyncReport, String> {
        let (persisted_items, persist_report) =
            Self::prepare_queue_items_for_persistence(items)?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("queue tx begin: {e}"))?;

        let mut statement = tx
            .prepare("SELECT id, item_json, position FROM conversion_queue")
            .map_err(|e| format!("queue reconcile prepare existing rows: {e}"))?;
        let existing_rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| format!("queue reconcile query existing rows: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("queue reconcile decode existing row: {e}"))?;
        drop(statement);

        let mut existing = existing_rows
            .into_iter()
            .map(|(id, json, position)| (id, (json, position)))
            .collect::<std::collections::HashMap<_, _>>();
        let mut report = QueueSyncReport::default();

        for (position, item) in persisted_items.iter().enumerate() {
            let json = serde_json::to_string(item)
                .map_err(|e| format!("queue item serialize: {e}"))?;
            match existing.remove(&item.id) {
                Some((old_json, old_position)) => {
                    let old_reference = Self::persisted_queue_reference(&old_json);
                    if old_reference.as_deref() != item.archive_password_ref.as_deref() {
                        if let Some(reference) = old_reference {
                            report.retire_references.push(reference);
                        }
                    }
                    if old_json != json || old_position != position as i64 {
                        tx.execute(
                            "INSERT INTO conversion_queue (id, item_json, position)
                             VALUES (?1, ?2, ?3)
                             ON CONFLICT(id) DO UPDATE SET
                                item_json = excluded.item_json,
                                position = excluded.position",
                            params![item.id, json, position as i64],
                        )
                        .map_err(|e| format!("queue item upsert: {e}"))?;
                        report.rows_written += 1;
                    }
                }
                None => {
                    tx.execute(
                        "INSERT INTO conversion_queue (id, item_json, position)
                         VALUES (?1, ?2, ?3)",
                        params![item.id, json, position as i64],
                    )
                    .map_err(|e| format!("queue item insert: {e}"))?;
                    report.rows_written += 1;
                }
            }
        }

        for (id, (old_json, _)) in existing {
            if let Some(reference) = Self::persisted_queue_reference(&old_json) {
                report.retire_references.push(reference);
            }
            tx.execute("DELETE FROM conversion_queue WHERE id = ?1", [&id])
                .map_err(|e| format!("queue item delete: {e}"))?;
            report.rows_deleted += 1;
        }

        tx.commit().map_err(|e| format!("queue tx commit: {e}"))?;

        let mut immediately_retirable = persist_report.retire_references;
        Self::dedup_unreferenced_queue_secret_refs(
            &mut immediately_retirable,
            &persisted_items,
        );
        crate::convert::queue::retire_queue_owned_secret_references(
            &immediately_retirable,
        );
        Self::dedup_unreferenced_queue_secret_refs(
            &mut report.retire_references,
            &persisted_items,
        );
        report.live_references = Self::durable_queue_secret_refs(&persisted_items);
        Ok(report)
    }

    /// Full-snapshot publication retained for imports, repair tooling, and
    /// tests. Ordinary application saves use `sync_queue` above.
    pub fn sync_queue_snapshot(
        &self,
        items: &[&crate::convert::ConversionItem],
    ) -> Result<QueueSyncReport, String> {
        let (persisted_items, persist_report) =
            Self::prepare_queue_items_for_persistence(items)?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("queue snapshot tx begin: {e}"))?;

        let mut old_statement = tx
            .prepare("SELECT item_json FROM conversion_queue")
            .map_err(|e| format!("queue snapshot read old refs: {e}"))?;
        let old_json = old_statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| format!("queue snapshot query old refs: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("queue snapshot decode old row: {e}"))?;
        drop(old_statement);
        let old_row_count = old_json.len();
        let mut retire_references = old_json
            .iter()
            .filter_map(|json| Self::persisted_queue_reference(json))
            .collect::<Vec<_>>();

        tx.execute("DELETE FROM conversion_queue", [])
            .map_err(|e| format!("queue snapshot clear: {e}"))?;
        for (position, item) in persisted_items.iter().enumerate() {
            let json = serde_json::to_string(item)
                .map_err(|e| format!("queue item serialize: {e}"))?;
            tx.execute(
                "INSERT INTO conversion_queue (id, item_json, position) VALUES (?1, ?2, ?3)",
                params![item.id, json, position as i64],
            )
            .map_err(|e| format!("queue snapshot insert: {e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("queue snapshot tx commit: {e}"))?;
        let mut immediately_retirable = persist_report.retire_references;
        Self::dedup_unreferenced_queue_secret_refs(
            &mut immediately_retirable,
            &persisted_items,
        );
        crate::convert::queue::retire_queue_owned_secret_references(
            &immediately_retirable,
        );
        Self::dedup_unreferenced_queue_secret_refs(
            &mut retire_references,
            &persisted_items,
        );
        Ok(QueueSyncReport {
            rows_written: persisted_items.len(),
            rows_deleted: old_row_count,
            retire_references,
            live_references: Self::durable_queue_secret_refs(&persisted_items),
        })
    }

    pub fn queue_legacy_import_done(&self) -> Result<bool, String> {
        self.legacy_import_flag("queue_import_done")
    }

    /// Publish the one-time legacy JSON queue import and its marker in the same
    /// SQLite transaction. Existing SQLite rows always win over legacy JSON.
    pub fn publish_legacy_queue_import(
        &self,
        items: &[crate::convert::ConversionItem],
        retire_after_import: &[String],
    ) -> Result<LegacyImportPublication, String> {
        // Fast no-op before secret preparation. The transaction below still
        // rechecks the marker to close a concurrent-start race, but ordinary
        // restarts must not touch secret storage once import is complete.
        if self.queue_legacy_import_done()? {
            return Ok(LegacyImportPublication::AlreadyDone);
        }

        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("queue legacy import tx begin: {e}"))?;

        let done = Self::legacy_import_flag_on(&tx, "queue_import_done")?;
        if done {
            tx.commit()
                .map_err(|e| format!("queue legacy import no-op commit: {e}"))?;
            return Ok(LegacyImportPublication::AlreadyDone);
        }

        let existing: i64 = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM conversion_queue LIMIT 1)",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("queue legacy import probe existing rows: {e}"))?;
        if existing != 0 {
            tx.execute(
                "UPDATE legacy_json_import_state SET queue_import_done = 1 WHERE id = ?1",
                [LEGACY_IMPORT_STATE_ROW_ID],
            )
            .map_err(|e| format!("queue legacy import mark existing authority: {e}"))?;
            tx.commit()
                .map_err(|e| format!("queue legacy import authority commit: {e}"))?;
            return Ok(LegacyImportPublication::ExistingSqliteAuthority);
        }

        // Claim SQLite's write reservation before secret preparation. Two app
        // processes can observe a pending marker concurrently; the loser must
        // fail here, before it can overwrite a stable secret reference that
        // belongs to the winner's committed import. The update is intentionally
        // value-preserving and remains part of the import transaction.
        tx.execute(
            "UPDATE legacy_json_import_state
             SET queue_import_done = queue_import_done
             WHERE id = ?1",
            [LEGACY_IMPORT_STATE_ROW_ID],
        )
        .map_err(|e| format!("queue legacy import claim authority: {e}"))?;

        let refs = items.iter().collect::<Vec<_>>();
        let (persisted_items, persist_report) =
            Self::prepare_queue_items_for_persistence(&refs)?;

        for (position, item) in persisted_items.iter().enumerate() {
            let json = serde_json::to_string(item)
                .map_err(|e| format!("queue legacy import serialize: {e}"))?;
            tx.execute(
                "INSERT INTO conversion_queue (id, item_json, position) VALUES (?1, ?2, ?3)",
                params![item.id, json, position as i64],
            )
            .map_err(|e| format!("queue legacy import insert: {e}"))?;
        }
        tx.execute(
            "UPDATE legacy_json_import_state SET queue_import_done = 1 WHERE id = ?1",
            [LEGACY_IMPORT_STATE_ROW_ID],
        )
        .map_err(|e| format!("queue legacy import mark done: {e}"))?;
        tx.commit()
            .map_err(|e| format!("queue legacy import commit: {e}"))?;

        let mut retire = persist_report.retire_references;
        retire.extend(retire_after_import.iter().cloned());
        Self::dedup_unreferenced_queue_secret_refs(&mut retire, &persisted_items);
        crate::convert::queue::retire_queue_owned_secret_references(&retire);
        Ok(LegacyImportPublication::Imported)
    }

    /// Load all queue items from SQLite in explicit durable order. Whole-query
    /// failures are returned to the caller; malformed individual rows are
    /// salvaged by omission and reconciled transactionally afterward.
    pub fn load_queue_items(&self) -> Result<QueueLoadOutcome, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, item_json FROM conversion_queue ORDER BY position ASC, id ASC",
            )
            .map_err(|e| format!("queue load prepare: {e}"))?;

        let mut rows = stmt
            .query([])
            .map_err(|e| format!("queue load query: {e}"))?;

        let mut maintenance_needed = false;
        let mut retire_after_maintenance = Vec::new();
        let mut items = Vec::<crate::convert::ConversionItem>::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| format!("queue load step: {e}"))?
        {
            let id = match row.get::<_, String>(0) {
                Ok(id) => id,
                Err(error) => {
                    maintenance_needed = true;
                    log::error!(
                        "Discarding persisted SQLite queue row with unreadable id: {error}"
                    );
                    continue;
                }
            };
            let json = match row.get::<_, String>(1) {
                Ok(json) => json,
                Err(error) => {
                    maintenance_needed = true;
                    log::error!(
                        "Discarding persisted SQLite queue row '{id}' with unreadable payload: {error}"
                    );
                    continue;
                }
            };
            match serde_json::from_str::<crate::convert::ConversionItem>(&json) {
                Ok(mut item) => {
                    if item.id != id {
                        maintenance_needed = true;
                        log::error!(
                            "Discarding persisted SQLite queue row whose primary key '{}' disagrees with payload id '{}'",
                            id,
                            item.id
                        );
                        if let Some(reference) = item.archive_password_ref.take() {
                            retire_after_maintenance.push(reference);
                        }
                        continue;
                    }
                    if matches!(item.status, crate::convert::ConversionStatus::Processing { .. }) {
                        item.status = crate::convert::ConversionStatus::Interrupted;
                        item.started_at = None;
                        item.completed_at = None;
                        maintenance_needed = true;
                    }
                    if crate::convert::queue_expansion::is_synthetic_cue_album_artifact(
                        &item.input_path,
                    ) {
                        maintenance_needed = true;
                        if let Some(reference) = item.archive_password_ref.take() {
                            retire_after_maintenance.push(reference);
                        }
                        log::warn!(
                            "Discarding synthetic CUE artifact from persisted SQLite queue: {:?}",
                            item.input_path
                        );
                        continue;
                    }
                    items.push(item);
                }
                Err(error) => {
                    maintenance_needed = true;
                    log::error!(
                        "Discarding malformed persisted SQLite queue row '{}': {}",
                        id,
                        error
                    );
                }
            }
        }
        drop(rows);
        drop(stmt);

        let secret_report =
            crate::convert::queue::restore_archive_passwords_after_load(&mut items);
        maintenance_needed |= secret_report.rewrite_required;
        let mut degradation = None;
        if maintenance_needed {
            let refs = items.iter().collect::<Vec<_>>();
            match self.sync_queue(&refs) {
                Ok(sync_report) => {
                    retire_after_maintenance.extend(secret_report.retire_references);
                    retire_after_maintenance.extend(sync_report.retire_references);
                    crate::convert::queue::retire_queue_owned_secret_references(
                        &retire_after_maintenance,
                    );
                }
                Err(error) => {
                    let message = format!(
                        "SQLite queue rows were loaded and sanitized in memory, but the maintenance publication failed: {error}"
                    );
                    log::error!("{message}");
                    degradation = Some(message);
                }
            }
        }
        Ok(QueueLoadOutcome { items, degradation })
    }

    /// Check if the queue table has any rows without collapsing read errors to
    /// an empty authority.
    pub fn has_queue_items(&self) -> Result<bool, String> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM conversion_queue LIMIT 1)",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)
            .map_err(|e| format!("queue authority probe: {e}"))
    }

    // ── Conversion history ───────────────────────────────────────

    /// Record a completed (or failed) conversion in the history table.
    pub fn record_conversion(
        &self,
        input_path: &str,
        output_path: Option<&str>,
        input_format: Option<&str>,
        output_format: &str,
        sample_rate: Option<u32>,
        bit_depth: Option<&str>,
        dither: Option<&str>,
        replaygain_mode: Option<&str>,
        source_size: Option<u64>,
        output_size: Option<u64>,
        queued_at: Option<&str>,
        started_at: Option<&str>,
        completed_at: &str,
        success: bool,
        error_message: Option<&str>,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO conversion_history (
                input_path, output_path, input_format, output_format,
                sample_rate, bit_depth, dither, replaygain_mode,
                source_size, output_size,
                queued_at, started_at, completed_at,
                success, error_message
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15
            )",
                params![
                    input_path,
                    output_path,
                    input_format,
                    output_format,
                    sample_rate,
                    bit_depth,
                    dither,
                    replaygain_mode,
                    source_size.map(|s| s as i64),
                    output_size.map(|s| s as i64),
                    queued_at,
                    started_at,
                    completed_at,
                    success as i32,
                    error_message,
                ],
            )
            .map_err(|e| format!("history insert: {}", e))?;
        Ok(())
    }

    /// Check if a file (by path) has been successfully converted before.
    /// For dedup warnings.
    pub fn was_previously_converted(&self, input_path: &str) -> bool {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM conversion_history
             WHERE input_path = ?1 AND success = 1",
                params![input_path],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0
    }

    // ── Presets ───────────────────────────────────────────────────

    /// List all presets grouped by format. Returns (format, Vec<name>)
    /// sorted by format then name. Instant via indexed query.
    pub fn list_presets_by_format(&self) -> Vec<(String, Vec<String>)> {
        let mut stmt = match self
            .conn
            .prepare("SELECT name, format FROM presets ORDER BY format, name")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        });

        if let Ok(rows) = rows {
            for row in rows.flatten() {
                let (name, format) = row;
                if let Some(group) = groups.iter_mut().find(|(f, _)| f == &format) {
                    group.1.push(name);
                } else {
                    groups.push((format, vec![name]));
                }
            }
        }
        groups
    }

    /// List all preset names (sorted).
    pub fn list_preset_names(&self) -> Vec<String> {
        let mut stmt = match self.conn.prepare("SELECT name FROM presets ORDER BY name") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        stmt.query_map([], |row| row.get::<_, String>(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Store a preset (upsert).
    pub fn store_preset(
        &self,
        name: &str,
        format: &str,
        description: Option<&str>,
        sample_rate: Option<u32>,
        bit_depth: Option<&str>,
        dither: Option<&str>,
        replaygain: Option<&str>,
        folder_template: Option<&str>,
        filename_template: Option<&str>,
        merge: Option<&str>,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO presets (
                name, format, description, sample_rate, bit_depth,
                dither, replaygain, folder_template, filename_template, merge
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    name,
                    format,
                    description,
                    sample_rate,
                    bit_depth,
                    dither,
                    replaygain,
                    folder_template,
                    filename_template,
                    merge,
                ],
            )
            .map_err(|e| format!("preset store: {}", e))?;
        Ok(())
    }

    /// Delete a preset by name.
    pub fn delete_preset(&self, name: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM presets WHERE name = ?1", params![name])
            .map_err(|e| format!("preset delete: {}", e))?;
        Ok(())
    }

    /// Check if the presets table has any entries.
    pub fn has_presets(&self) -> bool {
        self.conn
            .query_row("SELECT COUNT(*) FROM presets", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or(0)
            > 0
    }

    // ── Metadata journal ─────────────────────────────────────────

    /// Record an in-flight legacy metadata write. Called BEFORE the actual
    /// write. This DB table models the full-file `.tonepoet-bak` rollback
    /// path used by non-FLAC fallback writers. Native FLAC metadata writes do
    /// not enter this table because their recovery artifact is the adjacent
    /// `.tonepoet-meta-journal` that stores the original FLAC metadata region
    /// plus the intended replacement metadata-region identity.
    pub fn begin_metadata_write(&self, file_path: &str, backup_path: &str) -> Result<(), String> {
        self.begin_metadata_write_with_state(file_path, backup_path, METADATA_STATE_PREPARED)
    }

    fn begin_metadata_write_with_state(
        &self,
        file_path: &str,
        backup_path: &str,
        state: &str,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO metadata_journal (file_path, backup_path, started_at, state)
             VALUES (?1, ?2, ?3, ?4)",
                params![
                    file_path,
                    backup_path,
                    chrono::Utc::now().to_rfc3339(),
                    state,
                ],
            )
            .map_err(|e| {
                format!(
                    "journal insert refused for '{file_path}': an unresolved metadata write already owns this path or the journal is unavailable: {e}"
                )
            })?;
        record_test_metadata_journal_write();
        Ok(())
    }

    fn set_metadata_write_state(&self, file_path: &str, state: &str) -> Result<(), String> {
        let changed = self
            .conn
            .execute(
                "UPDATE metadata_journal SET state = ?2 WHERE file_path = ?1",
                params![file_path, state],
            )
            .map_err(|e| format!("journal state update for '{file_path}' to '{state}': {e}"))?;
        if changed != 1 {
            return Err(format!(
                "journal state update for '{file_path}' to '{state}' changed {changed} rows; expected exactly one"
            ));
        }
        record_test_metadata_journal_write();
        Ok(())
    }

    /// Remove the journal entry after backup cleanup reaches a terminal state.
    pub fn complete_metadata_write(&self, file_path: &str) -> Result<(), String> {
        let changed = self
            .conn
            .execute(
                "DELETE FROM metadata_journal WHERE file_path = ?1",
                params![file_path],
            )
            .map_err(|e| format!("journal delete for '{file_path}': {e}"))?;
        if changed != 1 {
            return Err(format!(
                "journal delete for '{file_path}' removed {changed} rows; expected exactly one"
            ));
        }
        record_test_metadata_journal_write();
        Ok(())
    }

    fn metadata_journal_entry(&self, file_path: &str) -> Result<Option<MetadataJournalEntry>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT file_path, backup_path, started_at, state
                 FROM metadata_journal WHERE file_path = ?1",
            )
            .map_err(|e| format!("journal lookup for '{file_path}': {e}"))?;
        let mut rows = stmt
            .query(params![file_path])
            .map_err(|e| format!("journal lookup for '{file_path}': {e}"))?;
        let Some(row) = rows
            .next()
            .map_err(|e| format!("journal lookup for '{file_path}': {e}"))?
        else {
            return Ok(None);
        };
        Ok(Some(MetadataJournalEntry {
            file_path: row.get(0).map_err(|e| format!("journal file_path decode: {e}"))?,
            backup_path: row.get(1).map_err(|e| format!("journal backup_path decode: {e}"))?,
            started_at: row.get(2).map_err(|e| format!("journal started_at decode: {e}"))?,
            state: row.get(3).map_err(|e| format!("journal state decode: {e}"))?,
        }))
    }

    fn metadata_journal_entries(&self) -> Result<Vec<MetadataJournalEntry>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT file_path, backup_path, started_at, state
                 FROM metadata_journal ORDER BY file_path",
            )
            .map_err(|e| format!("journal query: {e}"))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(MetadataJournalEntry {
                    file_path: row.get(0)?,
                    backup_path: row.get(1)?,
                    started_at: row.get(2)?,
                    state: row.get(3)?,
                })
            })
            .map_err(|e| format!("journal query: {e}"))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| format!("journal row decode: {e}"))?);
        }
        Ok(entries)
    }

    /// Compatibility view used by diagnostics and existing tests.
    pub fn stale_metadata_writes(&self) -> Result<Vec<(String, String, String)>, String> {
        Ok(self
            .metadata_journal_entries()?
            .into_iter()
            .map(|entry| (entry.file_path, entry.backup_path, entry.started_at))
            .collect())
    }

    // ── Browse directory-summary cache ───────────────────────────

    /// Look up a cached Browse directory summary by focused-directory
    /// identity. Returns None if the row is absent, stale, or corrupt.
    pub fn get_cached_directory_summary(
        &self,
        dir_path: &std::path::Path,
        identity: crate::tui::browse::ProbeCacheIdentity,
    ) -> Option<crate::tui::browse::DirectorySummaryCacheEntry> {
        let dir_path_key = dir_path.display().to_string();
        let size_i64 = directory_summary_identity_size_i64(identity.size);
        let mtime_nanos = directory_summary_identity_mtime_nanos(identity);
        let payload: String = self
            .conn
            .query_row(
                "SELECT payload FROM directory_summary_cache
                 WHERE dir_path = ?1 AND identity_size = ?2 AND identity_mtime_nanos = ?3",
                params![dir_path_key, size_i64, mtime_nanos],
                |row| row.get(0),
            )
            .ok()?;

        let (_path, entry) = crate::tui::browse::DirectorySummaryCacheEntry::from_persistent_line(&payload)?;
        if !entry.is_valid_for(identity) {
            return None;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let _ = self.conn.execute(
            "UPDATE directory_summary_cache SET accessed_at = ?1 WHERE dir_path = ?2",
            params![now, dir_path.display().to_string()],
        );
        Some(entry)
    }

    /// Store a scoped Browse directory summary. The serialized payload retains
    /// the cache-scope semantics from `DirectorySummaryFacts`, so future reads
    /// can distinguish immediate/depth-2 facts from best-effort recursive
    /// statistics instead of treating all rows as strong subtree fingerprints.
    pub fn store_directory_summary(
        &self,
        dir_path: &std::path::Path,
        entry: &crate::tui::browse::DirectorySummaryCacheEntry,
    ) -> Result<(), String> {
        let dir_path_key = dir_path.display().to_string();
        let size_i64 = directory_summary_identity_size_i64(entry.identity.size);
        let mtime_nanos = directory_summary_identity_mtime_nanos(entry.identity);
        let payload = entry.to_persistent_line(dir_path);
        let strongest_scope = directory_summary_strongest_scope_code(entry);
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO directory_summary_cache (
                    dir_path, identity_size, identity_mtime_nanos,
                    strongest_scope, payload, cached_at, accessed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(dir_path) DO UPDATE SET
                    identity_size = excluded.identity_size,
                    identity_mtime_nanos = excluded.identity_mtime_nanos,
                    strongest_scope = excluded.strongest_scope,
                    payload = excluded.payload,
                    cached_at = excluded.cached_at,
                    accessed_at = excluded.accessed_at",
                params![
                    dir_path_key,
                    size_i64,
                    mtime_nanos,
                    strongest_scope,
                    payload,
                    now,
                ],
            )
            .map_err(|e| format!("directory summary cache store: {}", e))?;
        Ok(())
    }

    /// Remove a persisted Browse directory summary for a path.
    pub fn invalidate_directory_summary(&self, dir_path: &std::path::Path) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM directory_summary_cache WHERE dir_path = ?1",
                params![dir_path.display().to_string()],
            )
            .map_err(|e| format!("directory summary cache invalidate: {}", e))?;
        Ok(())
    }

    // ── Probe cache ──────────────────────────────────────────────

    /// Look up a cached probe result. Returns None if not cached or
    /// if the file's mtime/size don't match (stale cache).
    pub fn get_cached_probe(
        &self,
        file_path: &str,
        current_mtime: i64,
        current_size: u64,
    ) -> Option<CachedProbeRow> {
        self.conn
            .query_row(
                "SELECT * FROM probe_cache WHERE file_path = ?1
             AND file_mtime = ?2 AND file_size = ?3",
                params![file_path, current_mtime, current_size as i64],
                cached_probe_row_from_sql,
            )
            .ok()
    }

    /// Batch-load valid probe-cache rows for a directory scan. SQLite's primary
    /// key handles each `IN` member as an indexed lookup, but doing this in
    /// chunks avoids one prepare/step cycle per cursor movement in large dirs.
    pub fn get_cached_probes_for_files(
        &self,
        files: &[(String, i64, u64)],
    ) -> Vec<(String, CachedProbeRow)> {
        if files.is_empty() {
            return Vec::new();
        }

        let mut expected = std::collections::HashMap::with_capacity(files.len());
        for (path, mtime, size) in files {
            expected.insert(path.clone(), (*mtime, *size));
        }

        let mut out = Vec::new();
        for chunk in files.chunks(900) {
            let placeholders = std::iter::repeat("?")
                .take(chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT * FROM probe_cache WHERE file_path IN ({})",
                placeholders
            );
            let Ok(mut stmt) = self.conn.prepare(&sql) else {
                continue;
            };
            let params = rusqlite::params_from_iter(chunk.iter().map(|(path, _, _)| path.as_str()));
            let Ok(rows) = stmt.query_map(params, |row| {
                let file_path: String = row.get("file_path")?;
                let file_mtime: i64 = row.get("file_mtime")?;
                let file_size_i64: i64 = row.get("file_size")?;
                let cached = cached_probe_row_from_sql(row)?;
                Ok((file_path, file_mtime, file_size_i64, cached))
            }) else {
                continue;
            };

            for row in rows.flatten() {
                let (file_path, file_mtime, file_size_i64, cached) = row;
                let file_size = u64::try_from(file_size_i64).unwrap_or(0);
                if expected
                    .get(&file_path)
                    .is_some_and(|(mtime, size)| *mtime == file_mtime && *size == file_size)
                {
                    out.push((file_path, cached));
                }
            }
        }

        out
    }

    /// Store a probe result in the cache (upsert).
    pub fn store_probe(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
        row: &CachedProbeRow,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO probe_cache (
                file_path, file_mtime, file_size,
                format_name, codec, bit_depth, sample_rate, channels,
                channel_layout, duration_secs,
                title, artist, album, genre, year, track_number, catalog_number,
                rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak,
                r128_track_gain, r128_album_gain,
                probed_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21, ?22, ?23, ?24
            )",
                params![
                    file_path,
                    mtime,
                    size as i64,
                    row.format_name,
                    row.codec,
                    row.bit_depth,
                    row.sample_rate,
                    row.channels,
                    row.channel_layout,
                    row.duration_secs,
                    row.title,
                    row.artist,
                    row.album,
                    row.genre,
                    row.year,
                    row.track_number,
                    row.catalog_number,
                    row.rg_track_gain,
                    row.rg_track_peak,
                    row.rg_album_gain,
                    row.rg_album_peak,
                    row.r128_track_gain,
                    row.r128_album_gain,
                    chrono::Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|e| format!("probe cache store: {}", e))?;
        Ok(())
    }

    /// Invalidate cache for a specific file (after metadata edit).
    pub fn invalidate_probe(&self, file_path: &str) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM probe_cache WHERE file_path = ?1",
                params![file_path],
            )
            .map_err(|e| format!("probe cache invalidate: {}", e))?;
        Ok(())
    }

    // ── Atomic metadata write ──────────────────────────────────

    /// Refuse a journal-free metadata replacement when any earlier recovery
    /// authority is still present. This is intentionally read-only: standard
    /// mode must neither allocate nor retire journal state, but it must not
    /// write bytes that startup recovery could later overwrite.
    pub fn assert_metadata_write_unarmed(
        &self,
        file_path: &std::path::Path,
    ) -> Result<(), String> {
        let path_str = file_path.display().to_string();
        if let Some(entry) = self.metadata_journal_entry(&path_str)? {
            return Err(format!(
                "metadata write refused for '{}': unresolved {} journal still owns rollback marker '{}' from {}; run startup recovery before retrying",
                file_path.display(),
                entry.state,
                entry.backup_path,
                entry.started_at,
            ));
        }
        let legacy_backup = Self::backup_path(file_path);
        if legacy_backup.exists() {
            return Err(format!(
                "metadata write refused: stale rollback marker '{}' already exists and will not be overwritten; run startup recovery before retrying",
                legacy_backup.display(),
            ));
        }
        Ok(())
    }

    /// Perform an atomic metadata write with an independent copy backup + journal.
    ///
    /// 1. Exclusively reserves an empty, non-authoritative unique marker
    /// 2. Records allocating ownership, then durably populates the backup
    /// 3. Records prepared state, then calls the provided write function
    /// 4. On success: durably syncs the destination and parent directory, records
    ///    committed state, then removes backup and journal
    /// 5. On write or durability failure: restores, records rolled-back state,
    ///    then retires both
    pub fn atomic_metadata_write<F>(
        &self,
        file_path: &std::path::Path,
        write_fn: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        self.atomic_metadata_write_with_durability(
            file_path,
            write_fn,
            Self::sync_metadata_destination,
        )
    }

    fn atomic_metadata_write_with_durability<F, S>(
        &self,
        file_path: &std::path::Path,
        write_fn: F,
        sync_fn: S,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
        S: FnOnce(&std::path::Path) -> Result<(), String>,
    {
        let path_str = file_path.display().to_string();
        let legacy_backup = Self::backup_path(file_path);

        // A previous operation that reached a terminal state may have failed
        // only during cleanup. Retire that state before allocating a new
        // rollback marker. A prepared state is still authoritative and must
        // block retries until recovery restores it.
        if let Some(entry) = self.metadata_journal_entry(&path_str)? {
            match entry.state.as_str() {
                METADATA_STATE_COMMITTED | METADATA_STATE_ROLLED_BACK => {
                    let recorded_backup = std::path::PathBuf::from(&entry.backup_path);
                    Self::remove_backup_marker(&recorded_backup)?;
                    self.complete_metadata_write(&path_str)?;
                }
                METADATA_STATE_ALLOCATING | METADATA_STATE_PREPARED => {
                    return Err(format!(
                        "metadata write refused for '{}': unresolved {} journal still owns rollback marker '{}' from {}",
                        file_path.display(),
                        entry.state.as_str(),
                        entry.backup_path,
                        entry.started_at
                    ));
                }
                other => {
                    return Err(format!(
                        "metadata write refused for '{}': journal has unknown state {:?}",
                        file_path.display(),
                        other
                    ));
                }
            }
        }

        // A deterministic marker belongs to the older standalone backup path.
        // It may be the only authoritative original copy from a failed write;
        // a database transaction must not bypass or overwrite it merely because
        // its own marker uses a unique operation suffix.
        if legacy_backup.exists() {
            return Err(format!(
                "backup refused: rollback marker '{}' already exists and will not be overwritten",
                legacy_backup.display()
            ));
        }

        // Open the source before reserving any marker. A missing or unreadable
        // destination therefore leaves no filesystem or journal artifact.
        let mut source = std::fs::File::open(file_path)
            .map_err(|error| format!("open '{}' for backup: {error}", file_path.display()))?;

        // Reserve an empty unique marker with create_new before journaling it.
        // A crash in this narrow window can leave only an empty, non-authoritative
        // orphan; the destination has not been mutated and no foreign marker can
        // ever be mistaken for this transaction's recovery authority.
        let (backup, mut destination) = Self::reserve_transaction_backup(file_path)?;
        let backup_str = backup.display().to_string();
        if let Err(journal_error) = self.begin_metadata_write_with_state(
            &path_str,
            &backup_str,
            METADATA_STATE_ALLOCATING,
        ) {
            drop(destination);
            return match Self::remove_backup_marker(&backup) {
                Ok(()) => Err(format!(
                    "journal error (write aborted before backup population): {journal_error}"
                )),
                Err(cleanup_error) => Err(format!(
                    "journal error (write aborted before backup population): {journal_error}; additionally could not remove the empty non-authoritative marker '{}': {cleanup_error}",
                    backup.display()
                )),
            };
        }

        let copied = match std::io::copy(&mut source, &mut destination) {
            Ok(copied) => copied,
            Err(error) => {
                drop(destination);
                let reason = format!(
                    "backup allocation failed for '{}': copy to rollback marker '{}': {error}",
                    file_path.display(),
                    backup.display()
                );
                return Err(self.abort_allocating_metadata_write(&path_str, &backup, reason));
            }
        };
        if let Err(error) = destination.sync_all() {
            drop(destination);
            let reason = format!(
                "backup allocation failed for '{}': sync rollback marker '{}': {error}",
                file_path.display(),
                backup.display()
            );
            return Err(self.abort_allocating_metadata_write(&path_str, &backup, reason));
        }
        record_test_metadata_backup_copy(copied);
        drop(destination);
        if let Err(error) = Self::sync_parent_directory(&backup) {
            let reason = format!(
                "backup allocation failed for '{}': rollback marker '{}' was populated, but parent-directory durability could not be confirmed: {error}",
                file_path.display(),
                backup.display()
            );
            return Err(self.abort_allocating_metadata_write(&path_str, &backup, reason));
        }

        if let Err(error) = self.set_metadata_write_state(&path_str, METADATA_STATE_PREPARED) {
            return match Self::remove_backup_marker(&backup) {
                Ok(()) => match self.complete_metadata_write(&path_str) {
                    Ok(()) => Err(format!(
                        "metadata write for '{}' was aborted before mutation because prepared state could not be recorded: {error}",
                        file_path.display()
                    )),
                    Err(journal_error) => Err(format!(
                        "metadata write for '{}' was aborted before mutation because prepared state could not be recorded: {error}; marker was removed, but allocating journal cleanup failed: {journal_error}",
                        file_path.display()
                    )),
                },
                Err(cleanup_error) => Err(format!(
                    "metadata write for '{}' was aborted before mutation because prepared state could not be recorded: {error}; allocating journal and marker '{}' remain because cleanup failed: {cleanup_error}",
                    file_path.display(),
                    backup.display()
                )),
            };
        }

        match write_fn() {
            Ok(()) => {
                if let Err(sync_error) = sync_fn(file_path) {
                    return self.rollback_prepared_metadata_write(
                        file_path,
                        &path_str,
                        &backup,
                        &backup_str,
                        format!("metadata durability sync failed: {sync_error}"),
                    );
                }
                // The write becomes committed only after the rewritten media
                // file and its parent directory are durable. A crash before
                // this update leaves prepared state, so recovery restores the
                // old bytes.
                if let Err(error) =
                    self.set_metadata_write_state(&path_str, METADATA_STATE_COMMITTED)
                {
                    return match Self::copy_backup_over(file_path, &backup) {
                        Ok(()) => Err(format!(
                            "metadata bytes were written to '{}', but commit authority could not be recorded: {error}; original bytes were restored and the prepared journal plus rollback marker remain armed for idempotent recovery",
                            file_path.display()
                        )),
                        Err(rollback_error) => Err(format!(
                            "metadata bytes were written to '{}', but commit authority could not be recorded: {error}; rollback also failed: {rollback_error}. Prepared journal and rollback marker '{}' remain armed",
                            file_path.display(),
                            backup.display()
                        )),
                    };
                }
                Self::remove_backup_marker(&backup).map_err(|error| {
                    format!(
                        "metadata write for '{}' committed, but rollback marker cleanup failed: {error}; committed journal remains armed and recovery must preserve the new bytes",
                        file_path.display()
                    )
                })?;
                self.complete_metadata_write(&path_str).map_err(|error| {
                    format!(
                        "metadata write for '{}' committed and rollback marker was removed, but journal cleanup failed: {error}; recovery must retire the committed journal without restoring old bytes",
                        file_path.display()
                    )
                })?;
                Ok(())
            }
            Err(write_error) => self.rollback_prepared_metadata_write(
                file_path,
                &path_str,
                &backup,
                &backup_str,
                write_error,
            ),
        }
    }

    fn rollback_prepared_metadata_write(
        &self,
        file_path: &std::path::Path,
        path_str: &str,
        backup: &std::path::Path,
        backup_str: &str,
        write_error: String,
    ) -> Result<(), String> {
        if !backup.exists() {
            return Err(format!(
                "write failed and rollback marker is missing: {write_error}. Expected backup at: {backup_str}. Prepared journal remains unresolved and blocks retries until the recovery failure is repaired explicitly"
            ));
        }

        Self::copy_backup_over(file_path, backup).map_err(|rollback_error| {
            format!(
                "write failed AND rollback could not be completed ({write_error}: {rollback_error}). Backup at: {backup_str}"
            )
        })?;
        self.set_metadata_write_state(path_str, METADATA_STATE_ROLLED_BACK)
            .map_err(|journal_error| {
                format!(
                    "write failed ({write_error}); original bytes were restored, but rollback completion could not be recorded: {journal_error}. Prepared journal and backup remain armed for idempotent recovery"
                )
            })?;
        Self::remove_backup_marker(backup).map_err(|cleanup_error| {
            format!(
                "write failed ({write_error}); original bytes were restored, but rollback marker cleanup failed: {cleanup_error}. Rolled-back journal remains armed"
            )
        })?;
        self.complete_metadata_write(path_str).map_err(|journal_error| {
            format!(
                "write failed ({write_error}); original bytes were restored and rollback marker removed, but journal cleanup failed: {journal_error}. Recovery must retire the rolled-back journal without changing the file"
            )
        })?;
        Err(format!("write failed (rolled back): {write_error}"))
    }

    fn sync_metadata_destination(file_path: &std::path::Path) -> Result<(), String> {
        #[cfg(windows)]
        let destination = std::fs::OpenOptions::new()
            .write(true)
            .open(file_path);
        #[cfg(not(windows))]
        let destination = std::fs::File::open(file_path);

        destination
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("sync rewritten media file '{}': {error}", file_path.display()))?;
        Self::sync_parent_directory(file_path).map_err(|error| {
            format!(
                "sync parent directory after metadata write to '{}': {error}",
                file_path.display()
            )
        })
    }

    /// Recover from any stale legacy DB journal entries. Native FLAC recovery
    /// is handled separately by the FLAC metadata-region journal scanner.
    /// Returns descriptions of recovered files.
    pub fn recover_stale_metadata_writes(&self) -> Vec<String> {
        let entries = match self.metadata_journal_entries() {
            Ok(entries) => entries,
            Err(error) => {
                return vec![format!(
                    "RECOVERY FAILED: metadata journal could not be read: {error}"
                )]
            }
        };

        let mut messages = Vec::new();
        for entry in entries {
            let backup = std::path::PathBuf::from(&entry.backup_path);
            let original = std::path::PathBuf::from(&entry.file_path);
            match entry.state.as_str() {
                METADATA_STATE_ALLOCATING => {
                    if let Err(error) = Self::remove_backup_marker(&backup) {
                        messages.push(format!(
                            "RECOVERY INCOMPLETE for {}: allocating state proves no writer started, but marker '{}' could not be retired without changing the destination: {}",
                            entry.file_path, entry.backup_path, error
                        ));
                        continue;
                    }
                    match self.complete_metadata_write(&entry.file_path) {
                        Ok(()) => messages.push(format!(
                            "Retired incomplete metadata backup allocation for {} without changing the file (operation started {})",
                            entry.file_path, entry.started_at
                        )),
                        Err(error) => messages.push(format!(
                            "RECOVERY INCOMPLETE for {}: allocating marker was retired without changing the file, but journal cleanup failed: {}",
                            entry.file_path, error
                        )),
                    }
                }
                METADATA_STATE_PREPARED => {
                    if !backup.exists() {
                        messages.push(format!(
                            "RECOVERY FAILED for {}: rollback marker is missing (write started {}); prepared journal remains unresolved and blocks retries",
                            entry.file_path, entry.started_at
                        ));
                        continue;
                    }

                    if let Err(error) = Self::copy_backup_over(&original, &backup) {
                        messages.push(format!(
                            "RECOVERY FAILED for {}: {}. Backup at: {}",
                            entry.file_path, error, entry.backup_path
                        ));
                        continue;
                    }
                    if let Err(error) =
                        self.set_metadata_write_state(&entry.file_path, METADATA_STATE_ROLLED_BACK)
                    {
                        messages.push(format!(
                            "RECOVERY INCOMPLETE for {}: original bytes were restored, but rollback completion could not be recorded: {}. Backup at: {}",
                            entry.file_path, error, entry.backup_path
                        ));
                        continue;
                    }
                    if let Err(error) = Self::remove_backup_marker(&backup) {
                        messages.push(format!(
                            "RECOVERY INCOMPLETE for {}: original bytes were restored, but rollback marker cleanup failed: {}",
                            entry.file_path, error
                        ));
                        continue;
                    }
                    match self.complete_metadata_write(&entry.file_path) {
                        Ok(()) => messages.push(format!(
                            "Recovered: {} (write started {})",
                            entry.file_path, entry.started_at
                        )),
                        Err(error) => messages.push(format!(
                            "RECOVERY INCOMPLETE for {}: original bytes were restored and rollback marker removed, but journal cleanup failed: {}",
                            entry.file_path, error
                        )),
                    }
                }
                METADATA_STATE_COMMITTED | METADATA_STATE_ROLLED_BACK => {
                    let state = entry.state.as_str();
                    if let Err(error) = Self::remove_backup_marker(&backup) {
                        messages.push(format!(
                            "RECOVERY INCOMPLETE for {}: terminal journal state '{}' must preserve current bytes, but rollback marker cleanup failed: {}",
                            entry.file_path, state, error
                        ));
                        continue;
                    }
                    match self.complete_metadata_write(&entry.file_path) {
                        Ok(()) => messages.push(format!(
                            "Finalized metadata journal for {} in state {} (write started {})",
                            entry.file_path, state, entry.started_at
                        )),
                        Err(error) => messages.push(format!(
                            "RECOVERY INCOMPLETE for {}: terminal journal state '{}' was preserved, but journal cleanup failed: {}",
                            entry.file_path, state, error
                        )),
                    }
                }
                other => messages.push(format!(
                    "RECOVERY FAILED for {}: unknown metadata journal state {:?}; no file or marker was changed",
                    entry.file_path, other
                )),
            }
        }
        messages
    }

    /// Backup path: same directory, `.tonepoet-bak` suffix.
    pub fn backup_path_for(original: &std::path::Path) -> std::path::PathBuf {
        Self::backup_path(original)
    }

    /// Create a backup (public entry point for async writes).
    pub fn create_backup_for(
        original: &std::path::Path,
        backup: &std::path::Path,
    ) -> Result<(), String> {
        Self::create_backup(original, backup)
    }

    /// Restore an independent full-file backup over an existing destination.
    ///
    /// `rename(backup, original)` cannot portably replace an existing path and
    /// can replace a symlink rather than restoring through it. Copying is the
    /// inverse of `create_backup`: it overwrites the destination's bytes while
    /// preserving the destination path identity. The rollback marker is removed
    /// only after the copy succeeds.
    pub fn restore_backup_for(
        original: &std::path::Path,
        backup: &std::path::Path,
    ) -> Result<(), String> {
        Self::copy_backup_over(original, backup)?;
        Self::remove_backup_marker(backup)
    }

    fn copy_backup_over(
        original: &std::path::Path,
        backup: &std::path::Path,
    ) -> Result<(), String> {
        // Read and validate the marker FIRST, and only replace the
        // destination with an atomic rename of a fully-written temp file.
        // Truncating the destination in place before the copy meant a bad
        // marker (directory, symlink, unreadable) destroyed the very bytes
        // this restore exists to recover.
        let restore_error = |detail: String| {
            format!(
                "restore '{}' from rollback marker '{}': {detail}",
                original.display(),
                backup.display()
            )
        };
        let marker_path_metadata = std::fs::symlink_metadata(backup)
            .map_err(|error| restore_error(error.to_string()))?;
        if marker_path_metadata.file_type().is_symlink() || !marker_path_metadata.is_file() {
            return Err(restore_error("marker is not a regular file".to_string()));
        }
        let mut marker = std::fs::File::open(backup)
            .map_err(|error| restore_error(error.to_string()))?;
        let marker_metadata = marker
            .metadata()
            .map_err(|error| restore_error(format!("inspect marker: {error}")))?;
        if !marker_metadata.is_file() {
            return Err(restore_error("marker is not a regular file".to_string()));
        }
        let marker_len = marker_metadata.len();
        let target = crate::config::resolve_config_save_target(original)
            .map_err(|error| restore_error(format!("resolve destination authority: {error}")))?;
        let target_metadata = std::fs::metadata(&target)
            .map_err(|error| restore_error(format!("inspect destination mode: {error}")))?;
        if !target_metadata.is_file() {
            return Err(restore_error(format!(
                "resolved destination '{}' is not a regular file",
                target.display()
            )));
        }
        let target_permissions = target_metadata.permissions();
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| restore_error("destination has no parent directory".to_string()))?;
        let mut temp = tempfile::Builder::new()
            .prefix(".metadata-restore.")
            .tempfile_in(parent)
            .map_err(|error| restore_error(format!("create restore temporary: {error}")))?;
        {
            use std::io::{Read, Write};
            const RESTORE_COPY_CHUNK_BYTES: usize = 1024 * 1024;
            let mut copied = 0u64;
            let mut buffer = vec![0u8; RESTORE_COPY_CHUNK_BYTES];
            while copied < marker_len {
                let wanted = usize::try_from((marker_len - copied).min(buffer.len() as u64))
                    .expect("bounded rollback-marker copy chunk");
                let read = marker
                    .read(&mut buffer[..wanted])
                    .map_err(|error| restore_error(format!("read rollback marker: {error}")))?;
                if read == 0 {
                    return Err(restore_error(format!(
                        "rollback marker ended after {copied} byte(s); expected {marker_len}"
                    )));
                }
                temp.as_file_mut()
                    .write_all(&buffer[..read])
                    .map_err(|error| restore_error(format!("write restore temporary: {error}")))?;
                copied += read as u64;
            }
            let mut trailing = [0u8; 1];
            if marker
                .read(&mut trailing)
                .map_err(|error| restore_error(format!("verify rollback-marker length: {error}")))?
                != 0
            {
                return Err(restore_error(
                    "rollback marker grew while it was being copied".to_string(),
                ));
            }
            temp.as_file()
                .set_permissions(target_permissions)
                .map_err(|error| restore_error(format!("preserve destination permissions: {error}")))?;
            temp.as_file()
                .sync_all()
                .map_err(|error| restore_error(format!("sync restore temporary: {error}")))?;
        }
        temp.persist(&target)
            .map_err(|error| restore_error(format!("publish restored bytes: {error}")))?;
        Self::sync_parent_directory(&target).map_err(|error| {
            restore_error(format!(
                "restored bytes were published, but parent-directory durability could not be confirmed: {error}"
            ))
        })?;
        Ok(())
    }

    fn remove_backup_marker(backup: &std::path::Path) -> Result<(), String> {
        match std::fs::remove_file(backup) {
            Ok(()) => Self::sync_parent_directory(backup).map_err(|error| {
                format!(
                    "rollback marker '{}' was removed, but parent-directory durability could not be confirmed: {error}",
                    backup.display()
                )
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove rollback marker '{}': {error}",
                backup.display()
            )),
        }
    }

    #[cfg(unix)]
    fn sync_parent_directory(path: &std::path::Path) -> std::io::Result<()> {
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        std::fs::File::open(parent)?.sync_all()
    }

    #[cfg(not(unix))]
    fn sync_parent_directory(_path: &std::path::Path) -> std::io::Result<()> {
        Ok(())
    }

    fn backup_path(original: &std::path::Path) -> std::path::PathBuf {
        let mut name = original
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into());
        name.push_str(".tonepoet-bak");
        original.with_file_name(name)
    }

    fn transaction_backup_path(original: &std::path::Path) -> std::path::PathBuf {
        let base = Self::backup_path(original);
        let file_name = base
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown.tonepoet-bak".to_string());
        base.with_file_name(format!("{file_name}.txn-{}", uuid::Uuid::new_v4()))
    }

    fn reserve_transaction_backup(
        original: &std::path::Path,
    ) -> Result<(std::path::PathBuf, std::fs::File), String> {
        const MAX_RESERVATION_ATTEMPTS: usize = 16;
        for _ in 0..MAX_RESERVATION_ATTEMPTS {
            let backup = Self::transaction_backup_path(original);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&backup)
            {
                Ok(file) => return Ok((backup, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "reserve unique rollback marker for '{}': {error}",
                        original.display()
                    ))
                }
            }
        }
        Err(format!(
            "reserve unique rollback marker for '{}': exhausted {MAX_RESERVATION_ATTEMPTS} collision-safe attempts",
            original.display()
        ))
    }

    fn abort_allocating_metadata_write(
        &self,
        file_path: &str,
        backup: &std::path::Path,
        reason: String,
    ) -> String {
        match Self::remove_backup_marker(backup) {
            Ok(()) => match self.complete_metadata_write(file_path) {
                Ok(()) => reason,
                Err(journal_error) => format!(
                    "{reason}; rollback marker was removed, but allocating journal cleanup failed: {journal_error}"
                ),
            },
            Err(cleanup_error) => format!(
                "{reason}; allocating journal and transaction-owned marker '{}' remain for recovery because marker cleanup failed: {cleanup_error}",
                backup.display()
            ),
        }
    }

    /// Create a backup by copying the file into a newly-created marker. We
    /// MUST copy, not hardlink, and MUST NOT overwrite an existing marker: an
    /// existing marker may be the only authoritative pre-write copy left by a
    /// failed rollback.
    fn create_backup(original: &std::path::Path, backup: &std::path::Path) -> Result<(), String> {
        let mut source = std::fs::File::open(original)
            .map_err(|error| format!("open '{}' for backup: {error}", original.display()))?;
        let mut destination = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(backup)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    format!(
                        "backup refused: rollback marker '{}' already exists and will not be overwritten",
                        backup.display()
                    )
                } else {
                    format!(
                        "create rollback marker '{}' for '{}': {error}",
                        backup.display(),
                        original.display()
                    )
                }
            })?;
        let copied = match std::io::copy(&mut source, &mut destination) {
            Ok(copied) => copied,
            Err(error) => {
                drop(destination);
                let cleanup = std::fs::remove_file(backup);
                let copy_error = format!(
                    "copy '{}' to rollback marker '{}': {error}",
                    original.display(),
                    backup.display()
                );
                return match cleanup {
                    Ok(()) => Err(copy_error),
                    Err(cleanup_error)
                        if cleanup_error.kind() == std::io::ErrorKind::NotFound =>
                    {
                        Err(copy_error)
                    }
                    Err(cleanup_error) => Err(format!(
                        "{copy_error}; additionally could not remove the incomplete marker: {cleanup_error}"
                    )),
                };
            }
        };
        if let Err(error) = destination.sync_all() {
            drop(destination);
            let cleanup = std::fs::remove_file(backup);
            let copy_error = format!(
                "sync rollback marker '{}' for '{}': {error}",
                backup.display(),
                original.display()
            );
            return match cleanup {
                Ok(()) => Err(copy_error),
                Err(cleanup_error)
                    if cleanup_error.kind() == std::io::ErrorKind::NotFound =>
                {
                    Err(copy_error)
                }
                Err(cleanup_error) => Err(format!(
                    "{copy_error}; additionally could not remove the incomplete marker: {cleanup_error}"
                )),
            };
        }
        record_test_metadata_backup_copy(copied);
        drop(destination);
        if let Err(error) = Self::sync_parent_directory(backup) {
            let cleanup = std::fs::remove_file(backup);
            return match cleanup {
                Ok(()) => Err(format!(
                    "rollback marker '{}' was written, but parent-directory durability could not be confirmed: {error}",
                    backup.display()
                )),
                Err(cleanup_error)
                    if cleanup_error.kind() == std::io::ErrorKind::NotFound =>
                {
                    Err(format!(
                        "rollback marker '{}' was written, but parent-directory durability could not be confirmed: {error}",
                        backup.display()
                    ))
                }
                Err(cleanup_error) => Err(format!(
                    "rollback marker '{}' was written, but parent-directory durability could not be confirmed: {error}; additionally could not remove the uncommitted marker: {cleanup_error}",
                    backup.display()
                )),
            };
        }
        Ok(())
    }

    // ── Recent files ─────────────────────────────────────────────

    fn legacy_import_flag(&self, flag: &str) -> Result<bool, String> {
        Self::legacy_import_flag_on(&self.conn, flag)
    }

    fn legacy_import_flag_on(conn: &Connection, flag: &str) -> Result<bool, String> {
        let column = match flag {
            "queue_import_done" => "queue_import_done",
            "recent_import_done" => "recent_import_done",
            _ => return Err(format!("unknown legacy import flag: {flag}")),
        };
        let sql = format!(
            "SELECT {column} FROM legacy_json_import_state WHERE id = ?1"
        );
        conn.query_row(&sql, [LEGACY_IMPORT_STATE_ROW_ID], |row| row.get::<_, i64>(0))
            .map(|value| value != 0)
            .map_err(|e| format!("read {column}: {e}"))
    }

    /// Record a file access (upsert with current timestamp).
    pub fn record_recent(&self, file_path: &str) -> Result<(), String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.record_recent_at(file_path, now)
    }

    /// Record a file access with a specific timestamp (for imports).
    pub fn record_recent_at(&self, file_path: &str, timestamp: i64) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("recent tx begin: {e}"))?;
        // REPLACE intentionally refreshes rowid. `accessed_at` has second
        // resolution, so rowid is the best in-schema tie-break signal for two
        // accesses in the same second (and for re-accessing an existing path).
        // Preserve access_count explicitly across the delete+insert semantics.
        tx.execute(
            "INSERT OR REPLACE INTO recent_files (file_path, accessed_at, access_count)
             VALUES (
                 ?1,
                 ?2,
                 COALESCE((
                     SELECT access_count + 1 FROM recent_files WHERE file_path = ?1
                 ), 1)
             )",
            params![file_path, timestamp],
        )
        .map_err(|e| format!("recent insert: {e}"))?;
        Self::prune_recent_rows(&tx, RECENT_FILES_RETENTION_LIMIT)?;
        tx.commit().map_err(|e| format!("recent tx commit: {e}"))?;
        Ok(())
    }

    fn prune_recent_rows(conn: &Connection, limit: usize) -> Result<(), String> {
        conn.execute(
            "DELETE FROM recent_files
             WHERE file_path IN (
                 SELECT file_path FROM recent_files
                 ORDER BY accessed_at DESC, rowid DESC, file_path ASC
                 LIMIT -1 OFFSET ?1
             )",
            [limit as i64],
        )
        .map_err(|e| format!("recent prune: {e}"))?;
        Ok(())
    }

    /// List recent files, most recent first, up to `limit`.
    pub fn list_recent(&self, limit: usize) -> Result<Vec<(String, i64)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT file_path, accessed_at FROM recent_files
             ORDER BY accessed_at DESC, rowid DESC, file_path ASC LIMIT ?1",
            )
            .map_err(|e| format!("recent query: {}", e))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| format!("recent query: {}", e))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(|e| format!("recent row decode: {e}"))?);
        }
        Ok(entries)
    }

    pub fn remove_recent(&self, file_path: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM recent_files WHERE file_path = ?1", [file_path])
            .map_err(|e| format!("recent delete: {e}"))?;
        Ok(())
    }

    pub fn recent_legacy_import_done(&self) -> Result<bool, String> {
        self.legacy_import_flag("recent_import_done")
    }

    pub fn has_recent_items(&self) -> Result<bool, String> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM recent_files LIMIT 1)",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)
            .map_err(|e| format!("recent authority probe: {e}"))
    }

    pub fn publish_legacy_recent_import(
        &self,
        entries: &[(String, i64)],
    ) -> Result<LegacyImportPublication, String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("recent legacy import tx begin: {e}"))?;

        let done = Self::legacy_import_flag_on(&tx, "recent_import_done")?;
        if done {
            tx.commit()
                .map_err(|e| format!("recent legacy import no-op commit: {e}"))?;
            return Ok(LegacyImportPublication::AlreadyDone);
        }

        let existing: i64 = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM recent_files LIMIT 1)",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("recent legacy import probe existing rows: {e}"))?;
        if existing != 0 {
            tx.execute(
                "UPDATE legacy_json_import_state SET recent_import_done = 1 WHERE id = ?1",
                [LEGACY_IMPORT_STATE_ROW_ID],
            )
            .map_err(|e| format!("recent legacy import mark existing authority: {e}"))?;
            tx.commit()
                .map_err(|e| format!("recent legacy import authority commit: {e}"))?;
            return Ok(LegacyImportPublication::ExistingSqliteAuthority);
        }

        // Claim the write reservation before reading the import payload into
        // SQLite. This makes two simultaneous first starts deterministic: only
        // one transaction can publish rows and advance the marker.
        tx.execute(
            "UPDATE legacy_json_import_state
             SET recent_import_done = recent_import_done
             WHERE id = ?1",
            [LEGACY_IMPORT_STATE_ROW_ID],
        )
        .map_err(|e| format!("recent legacy import claim authority: {e}"))?;

        // Legacy recent.json is stored newest-first. Insert oldest-first so
        // rowid DESC preserves that source order when timestamps tie.
        for (file_path, timestamp) in entries.iter().rev() {
            tx.execute(
                "INSERT OR REPLACE INTO recent_files (file_path, accessed_at, access_count)
                 VALUES (
                     ?1,
                     ?2,
                     COALESCE((
                         SELECT access_count + 1 FROM recent_files WHERE file_path = ?1
                     ), 1)
                 )",
                params![file_path, timestamp],
            )
            .map_err(|e| format!("recent legacy import insert: {e}"))?;
        }
        Self::prune_recent_rows(&tx, RECENT_FILES_RETENTION_LIMIT)?;
        tx.execute(
            "UPDATE legacy_json_import_state SET recent_import_done = 1 WHERE id = ?1",
            [LEGACY_IMPORT_STATE_ROW_ID],
        )
        .map_err(|e| format!("recent legacy import mark done: {e}"))?;
        tx.commit()
            .map_err(|e| format!("recent legacy import commit: {e}"))?;
        Ok(LegacyImportPublication::Imported)
    }

    // ── Bookmarks ────────────────────────────────────────────────

    /// List all bookmarks ordered by position.
    pub fn list_bookmarks(&self) -> Result<Vec<(i64, String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, path FROM bookmarks ORDER BY position ASC")
            .map_err(|e| format!("bookmarks query: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("bookmarks query: {}", e))?;

        let mut entries = Vec::new();
        for row in rows {
            if let Ok(entry) = row {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Add a bookmark at the end.
    pub fn add_bookmark(&self, name: &str, path: &str) -> Result<(), String> {
        let max_pos: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) FROM bookmarks",
                [],
                |row| row.get(0),
            )
            .unwrap_or(-1);

        self.conn
            .execute(
                "INSERT INTO bookmarks (name, path, position) VALUES (?1, ?2, ?3)",
                params![name, path, max_pos + 1],
            )
            .map_err(|e| format!("bookmark insert: {}", e))?;
        Ok(())
    }

    /// Atomically replace the complete bookmark compatibility mirror.
    ///
    /// Readers observe either the previous complete sequence or the new
    /// complete sequence. A process crash or insertion error cannot expose an
    /// empty/partial mirror because deletion and reinsertion commit together.
    pub fn replace_bookmarks_transactional(
        &self,
        bookmarks: &[(String, String)],
    ) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("bookmarks tx begin: {}", e))?;

        tx.execute("DELETE FROM bookmarks", [])
            .map_err(|e| format!("bookmarks clear: {}", e))?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO bookmarks (name, path, position) VALUES (?1, ?2, ?3)",
                )
                .map_err(|e| format!("bookmark insert prepare: {}", e))?;
            for (position, (name, path)) in bookmarks.iter().enumerate() {
                let position = i64::try_from(position)
                    .map_err(|_| "bookmark position exceeds SQLite integer range".to_string())?;
                insert
                    .execute(params![name, path, position])
                    .map_err(|e| {
                        format!("could not mirror bookmark '{}' ({}): {}", name, path, e)
                    })?;
            }
        }

        tx.commit()
            .map_err(|e| format!("bookmarks tx commit: {}", e))?;
        Ok(())
    }

    /// Clear all bookmarks (for full sync from in-memory state).
    pub fn clear_bookmarks(&self) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM bookmarks", [])
            .map_err(|e| format!("bookmarks clear: {}", e))?;
        Ok(())
    }

    /// Remove a bookmark by id.
    pub fn remove_bookmark(&self, id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM bookmarks WHERE id = ?1", params![id])
            .map_err(|e| format!("bookmark delete: {}", e))?;
        Ok(())
    }
}

/// A row from the probe_cache table, ready for conversion to SourceInfo + SourceMetadata.
#[derive(Debug, Clone, Default)]
pub struct CachedProbeRow {
    pub format_name: Option<String>,
    pub codec: Option<String>,
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub duration_secs: Option<f64>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub track_number: Option<u32>,
    pub catalog_number: Option<String>,
    pub rg_track_gain: Option<String>,
    pub rg_track_peak: Option<String>,
    pub rg_album_gain: Option<String>,
    pub rg_album_peak: Option<String>,
    pub r128_track_gain: Option<String>,
    pub r128_album_gain: Option<String>,
}

fn directory_summary_identity_size_i64(size: u64) -> i64 {
    i64::try_from(size).unwrap_or(i64::MAX)
}

fn directory_summary_identity_mtime_nanos(identity: crate::tui::browse::ProbeCacheIdentity) -> i64 {
    identity
        .modified
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| {
            let secs = i64::try_from(duration.as_secs()).unwrap_or(i64::MAX / 1_000_000_000);
            secs.saturating_mul(1_000_000_000)
                .saturating_add(i64::from(duration.subsec_nanos()))
        })
        .unwrap_or(-1)
}

fn directory_summary_scope_rank(scope: crate::tui::browse::DirectorySummaryScope) -> u8 {
    match scope {
        crate::tui::browse::DirectorySummaryScope::Immediate => 0,
        crate::tui::browse::DirectorySummaryScope::ShallowDepth2 => 1,
        crate::tui::browse::DirectorySummaryScope::RecursiveBestEffort => 2,
    }
}

fn directory_summary_scope_code(scope: crate::tui::browse::DirectorySummaryScope) -> &'static str {
    match scope {
        crate::tui::browse::DirectorySummaryScope::Immediate => "immediate",
        crate::tui::browse::DirectorySummaryScope::ShallowDepth2 => "shallow2",
        crate::tui::browse::DirectorySummaryScope::RecursiveBestEffort => "recursive-best-effort",
    }
}

fn directory_summary_strongest_scope_code(
    entry: &crate::tui::browse::DirectorySummaryCacheEntry,
) -> &'static str {
    let mut strongest = entry.facts.classification_scope;
    if let Some(stats_scope) = entry.facts.stats_scope {
        if strongest
            .map(|scope| directory_summary_scope_rank(stats_scope) > directory_summary_scope_rank(scope))
            .unwrap_or(true)
        {
            strongest = Some(stats_scope);
        }
    }
    directory_summary_scope_code(
        strongest.unwrap_or(crate::tui::browse::DirectorySummaryScope::Immediate),
    )
}

fn cached_probe_row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<CachedProbeRow> {
    Ok(CachedProbeRow {
        format_name: row.get("format_name")?,
        codec: row.get("codec")?,
        bit_depth: row.get("bit_depth")?,
        sample_rate: row.get("sample_rate")?,
        channels: row.get("channels")?,
        channel_layout: row.get("channel_layout")?,
        duration_secs: row.get("duration_secs")?,
        title: row.get("title")?,
        artist: row.get("artist")?,
        album: row.get("album")?,
        genre: row.get("genre")?,
        year: row.get("year")?,
        track_number: row.get("track_number")?,
        catalog_number: row.get("catalog_number")?,
        rg_track_gain: row.get("rg_track_gain")?,
        rg_track_peak: row.get("rg_track_peak")?,
        rg_album_gain: row.get("rg_album_gain")?,
        rg_album_peak: row.get("rg_album_peak")?,
        r128_track_gain: row.get("r128_track_gain")?,
        r128_album_gain: row.get("r128_album_gain")?,
    })
}

// ── Conversion helpers ──────────────────────────────────────────

impl CachedProbeRow {
    /// Convert a CachedProbeRow to a CachedInfo (SourceInfo + SourceMetadata).
    /// Returns None if essential fields (format_name, sample_rate, channels) are missing.
    pub fn to_cached_info(&self, file_size: u64) -> Option<crate::tui::browse::CachedInfo> {
        use crate::tui::probe::{SourceInfo, SourceMetadata};
        let source = SourceInfo {
            sample_format_is_float: None,
            format_name: self.format_name.clone()?,
            codec: self.codec.clone().unwrap_or_default(),
            bit_depth: self.bit_depth,
            sample_rate: self.sample_rate?,
            channels: self.channels?,
            channel_layout: self.channel_layout.clone().unwrap_or_default(),
            duration_secs: self.duration_secs.unwrap_or(0.0),
            file_size,
        };
        let metadata = SourceMetadata {
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            genre: self.genre.clone(),
            year: self.year.clone(),
            track_number: self.track_number,
            catalog_number: self.catalog_number.clone(),
            rg_track_gain: self.rg_track_gain.clone(),
            rg_track_peak: self.rg_track_peak.clone(),
            rg_album_gain: self.rg_album_gain.clone(),
            rg_album_peak: self.rg_album_peak.clone(),
            r128_track_gain: self.r128_track_gain.clone(),
            r128_album_gain: self.r128_album_gain.clone(),
            preemphasis_metadata: None, // Not cached; re-detected on probe.
            hdcd_detail: None,          // Populated from analysis cache if available.
            isrc: None, // Not cached; re-read on full probe (only used by :cue).
            tool: None, // Not cached; re-read on full probe.
            artwork: Vec::new(), // Not cached; re-read on full probe.
            embedded_cue_availability:
                crate::tui::probe::EmbeddedCueAvailability::Unknown,
        };
        Some(crate::tui::browse::CachedInfo::new(source, metadata))
    }

    /// Build a CachedProbeRow from SourceInfo + SourceMetadata.
    pub fn from_cached_info(info: &crate::tui::browse::CachedInfo) -> Self {
        Self {
            format_name: Some(info.source.format_name.clone()),
            codec: Some(info.source.codec.clone()),
            bit_depth: info.source.bit_depth,
            sample_rate: Some(info.source.sample_rate),
            channels: Some(info.source.channels),
            channel_layout: Some(info.source.channel_layout.clone()),
            duration_secs: Some(info.source.duration_secs),
            title: info.metadata.title.clone(),
            artist: info.metadata.artist.clone(),
            album: info.metadata.album.clone(),
            genre: info.metadata.genre.clone(),
            year: info.metadata.year.clone(),
            track_number: info.metadata.track_number,
            catalog_number: info.metadata.catalog_number.clone(),
            rg_track_gain: info.metadata.rg_track_gain.clone(),
            rg_track_peak: info.metadata.rg_track_peak.clone(),
            rg_album_gain: info.metadata.rg_album_gain.clone(),
            rg_album_peak: info.metadata.rg_album_peak.clone(),
            r128_track_gain: info.metadata.r128_track_gain.clone(),
            r128_album_gain: info.metadata.r128_album_gain.clone(),
        }
    }
}

/// Convert a SystemTime to a unix timestamp (seconds since epoch) as i64.
pub fn systemtime_to_unix(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ── Pre-emphasis corpus model storage ──────────────────────────────

impl Database {
    /// Load the pre-emphasis corpus model from the database.
    pub fn load_preemph_corpus(
        &self,
    ) -> Result<crate::tui::preemphasis::corpus::CorpusModel, String> {
        use crate::tui::preemphasis::stft::NUM_BANDS;

        let (n_frames, n_tracks, mean_blob, cov_blob, pca_blob, pe_tmpl_blob): (i64, i64, Vec<u8>, Vec<u8>, Vec<u8>, Option<Vec<u8>>) =
            self.conn.query_row(
                "SELECT n_frames, n_tracks, mean, covariance, pca, pe_template FROM preemph_corpus WHERE id = 1",
                [],
                |row| Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                )),
            ).map_err(|_| "no corpus model found (run :preemph-train)".to_string())?;

        // Deserialize mean (31 x f64 LE).
        if mean_blob.len() != NUM_BANDS * 8 {
            return Err("corrupt corpus mean blob".into());
        }
        let mut mean = [0.0f64; NUM_BANDS];
        for k in 0..NUM_BANDS {
            mean[k] = f64::from_le_bytes(mean_blob[k * 8..(k + 1) * 8].try_into().unwrap());
        }

        // Deserialize covariance (31x31 x f64 LE).
        let cov_size = NUM_BANDS * NUM_BANDS;
        if cov_blob.len() != cov_size * 8 {
            return Err("corrupt corpus covariance blob".into());
        }
        let mut covariance = vec![0.0f64; cov_size];
        for i in 0..cov_size {
            covariance[i] = f64::from_le_bytes(cov_blob[i * 8..(i + 1) * 8].try_into().unwrap());
        }

        // Deserialize PCA components (N x 31 x f64 LE).
        let pca_component_bytes = NUM_BANDS * 8;
        if pca_blob.len() % pca_component_bytes != 0 {
            return Err("corrupt corpus PCA blob".into());
        }
        let num_components = pca_blob.len() / pca_component_bytes;
        let mut pca_components = Vec::with_capacity(num_components);
        for c in 0..num_components {
            let mut pc = [0.0f64; NUM_BANDS];
            let offset = c * pca_component_bytes;
            for k in 0..NUM_BANDS {
                pc[k] = f64::from_le_bytes(
                    pca_blob[offset + k * 8..offset + (k + 1) * 8]
                        .try_into()
                        .unwrap(),
                );
            }
            pca_components.push(pc);
        }

        // Deserialize empirical PE template if present.
        let empirical_pe_template = pe_tmpl_blob.and_then(|blob| {
            if blob.len() != NUM_BANDS * 8 {
                return None;
            }
            let mut tmpl = [0.0f64; NUM_BANDS];
            for k in 0..NUM_BANDS {
                tmpl[k] = f64::from_le_bytes(blob[k * 8..(k + 1) * 8].try_into().ok()?);
            }
            Some(tmpl)
        });

        Ok(crate::tui::preemphasis::corpus::CorpusModel {
            mean,
            covariance,
            pca_components,
            empirical_pe_template,
            n_frames: n_frames as u64,
            n_tracks: n_tracks as u64,
        })
    }

    /// Store (or replace) the pre-emphasis corpus model.
    pub fn store_preemph_corpus(
        &self,
        model: &crate::tui::preemphasis::corpus::CorpusModel,
    ) -> Result<(), String> {
        use crate::tui::preemphasis::stft::NUM_BANDS;

        // Serialize mean.
        let mut mean_blob = Vec::with_capacity(NUM_BANDS * 8);
        for &v in &model.mean {
            mean_blob.extend_from_slice(&v.to_le_bytes());
        }

        // Serialize covariance.
        let mut cov_blob = Vec::with_capacity(model.covariance.len() * 8);
        for &v in &model.covariance {
            cov_blob.extend_from_slice(&v.to_le_bytes());
        }

        // Serialize PCA components.
        let mut pca_blob = Vec::with_capacity(model.pca_components.len() * NUM_BANDS * 8);
        for pc in &model.pca_components {
            for &v in pc {
                pca_blob.extend_from_slice(&v.to_le_bytes());
            }
        }

        // Serialize empirical PE template if present.
        let pe_tmpl_blob: Option<Vec<u8>> = model.empirical_pe_template.map(|tmpl| {
            let mut blob = Vec::with_capacity(NUM_BANDS * 8);
            for &v in &tmpl {
                blob.extend_from_slice(&v.to_le_bytes());
            }
            blob
        });

        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR REPLACE INTO preemph_corpus (id, n_frames, n_tracks, mean, covariance, pca, pe_template, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                model.n_frames as i64,
                model.n_tracks as i64,
                mean_blob,
                cov_blob,
                pca_blob,
                pe_tmpl_blob,
                now,
            ],
        ).map_err(|e| format!("store corpus: {}", e))?;
        Ok(())
    }

    /// Load the trained pre-emphasis LDA classifier from the database.
    pub fn load_preemph_classifier(
        &self,
    ) -> Result<crate::tui::preemphasis::scoring::LdaClassifier, String> {
        use crate::tui::preemphasis::scoring::NUM_FEATURES;

        let (weights_blob, bias, threshold, impute_blob, means_blob, stds_blob): (
            Vec<u8>,
            f64,
            f64,
            Vec<u8>,
            Vec<u8>,
            Vec<u8>,
        ) = self
            .conn
            .query_row(
                "SELECT weights, bias, threshold, feature_impute, feature_means, feature_stds
                 FROM preemph_classifier WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(|_| "no trained classifier found (run :preemph-calibrate)".to_string())?;

        let expected_len = NUM_FEATURES * 8;
        if weights_blob.len() != expected_len
            || impute_blob.len() != expected_len
            || means_blob.len() != expected_len
            || stds_blob.len() != expected_len
        {
            return Err("corrupt classifier blob".into());
        }

        let mut weights = [0.0f64; NUM_FEATURES];
        let mut feature_impute = [0.0f64; NUM_FEATURES];
        let mut feature_means = [0.0f64; NUM_FEATURES];
        let mut feature_stds = [0.0f64; NUM_FEATURES];

        for i in 0..NUM_FEATURES {
            weights[i] = f64::from_le_bytes(weights_blob[i * 8..(i + 1) * 8].try_into().unwrap());
            feature_impute[i] =
                f64::from_le_bytes(impute_blob[i * 8..(i + 1) * 8].try_into().unwrap());
            feature_means[i] =
                f64::from_le_bytes(means_blob[i * 8..(i + 1) * 8].try_into().unwrap());
            feature_stds[i] = f64::from_le_bytes(stds_blob[i * 8..(i + 1) * 8].try_into().unwrap());
        }

        Ok(crate::tui::preemphasis::scoring::LdaClassifier {
            weights,
            bias,
            threshold,
            feature_impute,
            feature_means,
            feature_stds,
        })
    }

    /// Store (or replace) the trained pre-emphasis LDA classifier.
    pub fn store_preemph_classifier(
        &self,
        classifier: &crate::tui::preemphasis::scoring::LdaClassifier,
        cv_accuracy: f64,
        cv_fpr: f64,
        cv_precision: f64,
    ) -> Result<(), String> {
        use crate::tui::preemphasis::scoring::NUM_FEATURES;

        let serialize = |arr: &[f64; NUM_FEATURES]| -> Vec<u8> {
            let mut blob = Vec::with_capacity(NUM_FEATURES * 8);
            for &v in arr {
                blob.extend_from_slice(&v.to_le_bytes());
            }
            blob
        };

        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "INSERT OR REPLACE INTO preemph_classifier
             (id, weights, bias, threshold, feature_impute, feature_means, feature_stds,
              cv_accuracy, cv_fpr, cv_precision, trained_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    serialize(&classifier.weights),
                    classifier.bias,
                    classifier.threshold,
                    serialize(&classifier.feature_impute),
                    serialize(&classifier.feature_means),
                    serialize(&classifier.feature_stds),
                    cv_accuracy,
                    cv_fpr,
                    cv_precision,
                    now,
                ],
            )
            .map_err(|e| format!("store classifier: {}", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_v23_prerequisite_tables(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE conversion_queue (
                id        TEXT PRIMARY KEY,
                item_json TEXT NOT NULL
             );
             CREATE TABLE recent_files (
                file_path    TEXT PRIMARY KEY,
                accessed_at  INTEGER NOT NULL,
                access_count INTEGER NOT NULL DEFAULT 1
             );",
        )
        .expect("create v23 prerequisite tables");
    }

    fn queue_item(
        id: &str,
        input_path: &str,
        status: crate::convert::ConversionStatus,
    ) -> crate::convert::ConversionItem {
        let mut item = crate::convert::ConversionItem::default();
        item.id = id.to_string();
        item.input_path = PathBuf::from(input_path);
        item.status = status;
        item
    }

    fn table_columns(conn: &Connection, table: &str) -> Vec<(String, String)> {
        let mut statement = conn
            .prepare(&format!("PRAGMA table_info({})", table))
            .expect("prepare table_info");
        statement
            .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
            .expect("query table_info")
            .collect::<Result<Vec<_>, _>>()
            .expect("decode table_info")
    }

    fn seed_schema_through_v8(conn: &Connection) {
        Database::migrate_v1(conn).expect("seed v1");
        Database::migrate_v2(conn).expect("seed v2");
        Database::migrate_v3(conn).expect("seed v3");
        Database::migrate_v4(conn).expect("seed v4");
        Database::migrate_v5(conn).expect("seed v5");
        Database::migrate_v6(conn).expect("seed v6");
        Database::migrate_v7(conn).expect("seed v7");
        Database::migrate_v8(conn).expect("seed v8");
        conn.pragma_update(None, "user_version", 8)
            .expect("stamp v8");
    }

    fn seed_schema_through_v9(conn: &Connection) {
        seed_schema_through_v8(conn);
        Database::migrate_v9(conn).expect("seed v9");
        conn.pragma_update(None, "user_version", 9)
            .expect("stamp v9");
    }

    #[test]
    fn open_and_migrate() {
        let db = Database::open_memory().unwrap();
        let version: u32 = db
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_VERSION);
    }

    const CROSS_PROCESS_DB_PATH_ENV: &str = "TONEPOET_DB_CROSS_PROCESS_PATH";
    const CROSS_PROCESS_DB_GATE_ENV: &str = "TONEPOET_DB_CROSS_PROCESS_GATE";
    const CROSS_PROCESS_DB_CHILD_ID_ENV: &str = "TONEPOET_DB_CROSS_PROCESS_CHILD_ID";
    const CROSS_PROCESS_DB_SUCCESS_DIR_ENV: &str = "TONEPOET_DB_CROSS_PROCESS_SUCCESS_DIR";

    #[test]
    fn cross_process_database_open_child() {
        let Some(db_path) = std::env::var_os(CROSS_PROCESS_DB_PATH_ENV).map(PathBuf::from) else {
            return;
        };
        let gate = PathBuf::from(
            std::env::var_os(CROSS_PROCESS_DB_GATE_ENV)
                .expect("cross-process DB child gate path"),
        );
        let child_id = std::env::var(CROSS_PROCESS_DB_CHILD_ID_ENV)
            .expect("cross-process DB child id");
        let success_dir = PathBuf::from(
            std::env::var_os(CROSS_PROCESS_DB_SUCCESS_DIR_ENV)
                .expect("cross-process DB child success directory"),
        );

        let gate_wait_started = std::time::Instant::now();
        while !gate.exists() {
            assert!(
                gate_wait_started.elapsed() < std::time::Duration::from_secs(10),
                "timed out waiting for parent start gate {}",
                gate.display(),
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        for round in 0..16 {
            let db = Database::open_path(&db_path).unwrap_or_else(|error| {
                panic!("child {child_id} round {round} could not open shared database: {error}")
            });
            let journal_mode: String = db
                .conn
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .expect("read child journal mode");
            assert_eq!(journal_mode.to_ascii_lowercase(), "wal");

            // Hold SQLite's writer reservation briefly. Other child processes
            // continue opening the already-WAL database during this window;
            // an open path must not attempt another journal-mode transition.
            db.conn
                .execute_batch("BEGIN IMMEDIATE;")
                .expect("acquire child SQLite writer reservation");
            std::thread::sleep(std::time::Duration::from_millis(10));
            db.conn
                .execute_batch("COMMIT;")
                .expect("release child SQLite writer reservation");

            let file_path = format!("/cross-process/{child_id}/{round}.wv");
            let backup_path = format!("{file_path}.tonepoet-bak");
            db.begin_metadata_write(&file_path, &backup_path)
                .unwrap_or_else(|error| {
                    panic!("child {child_id} round {round} journal insert failed: {error}")
                });
            db.complete_metadata_write(&file_path)
                .unwrap_or_else(|error| {
                    panic!("child {child_id} round {round} journal delete failed: {error}")
                });
        }

        std::fs::write(success_dir.join(format!("child-{child_id}.ok")), b"ok")
            .expect("publish cross-process DB child success marker");
    }

    #[test]
    fn concurrent_processes_open_and_write_one_wal_database() {
        const CHILD_COUNT: usize = 6;
        let temp = tempfile::tempdir().expect("cross-process database tempdir");
        let db_path = temp.path().join("tonepoet.db");
        let gate = temp.path().join("start");
        let success_dir = temp.path().join("success");
        std::fs::create_dir(&success_dir).expect("create child success directory");
        let current_exe = std::env::current_exe().expect("resolve current test executable");

        let mut children = Vec::with_capacity(CHILD_COUNT);
        for child_id in 0..CHILD_COUNT {
            let child = std::process::Command::new(&current_exe)
                .arg("--exact")
                .arg("db::tests::cross_process_database_open_child")
                .arg("--nocapture")
                .env(CROSS_PROCESS_DB_PATH_ENV, &db_path)
                .env(CROSS_PROCESS_DB_GATE_ENV, &gate)
                .env(CROSS_PROCESS_DB_CHILD_ID_ENV, child_id.to_string())
                .env(CROSS_PROCESS_DB_SUCCESS_DIR_ENV, &success_dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap_or_else(|error| panic!("spawn DB child {child_id}: {error}"));
            children.push((child_id, child));
        }

        std::fs::write(&gate, b"go").expect("release DB child start gate");
        for (child_id, child) in children {
            let output = child
                .wait_with_output()
                .unwrap_or_else(|error| panic!("wait for DB child {child_id}: {error}"));
            assert!(
                output.status.success(),
                "DB child {child_id} failed with {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            assert!(
                success_dir.join(format!("child-{child_id}.ok")).is_file(),
                "DB child {child_id} exited without running the filtered helper test",
            );
        }

        let db = Database::open_path(&db_path).expect("open database after process contention");
        let journal_mode: String = db
            .conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("read final journal mode");
        let version: u32 = db
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read final schema version");
        let pending_journal_rows: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM metadata_journal", [], |row| row.get(0))
            .expect("count final metadata journal rows");
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(version, CURRENT_VERSION);
        assert_eq!(pending_journal_rows, 0);
        assert!(
            Database::open_init_lock_path(&db_path).is_file(),
            "persistent DB initialization lock marker must remain adjacent to the database",
        );
    }

    #[test]
    fn failed_migration_step_rolls_back_schema_and_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create v9 database");
            conn.execute_batch(
                "CREATE TABLE analysis_cache (
                    file_path TEXT PRIMARY KEY,
                    preemphasis_corr TEXT
                 );
                 PRAGMA user_version = 9;",
            )
            .expect("seed incompatible v10 database");
        }

        let error = Database::open_path(&path)
            .err()
            .expect("v10 must reject incompatible column");
        assert!(
            error.contains(
                "v10 migration found incompatible existing column analysis_cache.preemphasis_corr"
            ),
            "unexpected migration error: {error}"
        );

        let conn = Connection::open(&path).expect("reopen failed migration database");
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read rolled-back version");
        let columns = table_columns(&conn, "analysis_cache");

        assert_eq!(version, 9, "failed v10 must not advance user_version");
        assert!(
            !columns.iter().any(|(name, _)| name == "preemphasis"),
            "v10's first ALTER must roll back when the second column is incompatible"
        );
        assert_eq!(
            columns
                .iter()
                .find(|(name, _)| name == "preemphasis_corr")
                .map(|(_, declared_type)| declared_type.as_str()),
            Some("TEXT"),
            "pre-existing incompatible column must remain untouched"
        );
    }

    #[test]
    fn resumes_after_committed_v8_without_rerunning_v8() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create v8 database");
            seed_schema_through_v8(&conn);
            conn.execute(
                "INSERT INTO analysis_cache (
                    file_path, file_mtime, file_size, algo_version, analyzed_at
                 ) VALUES (?1, 1, 1, 8, '2026-08-10T00:00:00Z')",
                ["/music/resume.flac"],
            )
            .expect("insert v8 sentinel row");
        }

        let db = Database::open_path(&path).expect("resume from committed v8");
        let version: u32 = db
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read resumed version");
        let sentinel_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM analysis_cache WHERE file_path = '/music/resume.flac'",
                [],
                |row| row.get(0),
            )
            .expect("read v8 sentinel row");

        assert_eq!(version, CURRENT_VERSION);
        assert_eq!(
            sentinel_count, 1,
            "resume must start at v9; rerunning destructive v8 would delete the sentinel"
        );
    }

    #[test]
    fn historical_partial_v10_add_column_resumes_without_data_loss() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create partial v10 database");
            seed_schema_through_v9(&conn);
            conn.execute(
                "INSERT INTO analysis_cache (
                    file_path, file_mtime, file_size, algo_version, analyzed_at
                 ) VALUES (?1, 1, 1, 8, '2026-08-10T00:00:00Z')",
                ["/music/partial-v10.flac"],
            )
            .expect("insert pre-v10 sentinel row");
            conn.execute_batch(
                "ALTER TABLE analysis_cache ADD COLUMN preemphasis INTEGER;
                 UPDATE analysis_cache SET preemphasis = 1 WHERE file_path = '/music/partial-v10.flac';
                 PRAGMA user_version = 9;",
            )
            .expect("seed historical partial v10");
        }

        let db = Database::open_path(&path).expect("finish historical partial v10");
        let version: u32 = db
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read migrated version");
        let preemphasis: Option<i64> = db
            .conn
            .query_row(
                "SELECT preemphasis FROM analysis_cache
                 WHERE file_path = '/music/partial-v10.flac'",
                [],
                |row| row.get(0),
            )
            .expect("read preserved partial-v10 row");
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM analysis_cache
                 WHERE file_path = '/music/partial-v10.flac'",
                [],
                |row| row.get(0),
            )
            .expect("count preserved partial-v10 row");
        let columns = table_columns(&db.conn, "analysis_cache");

        assert_eq!(version, CURRENT_VERSION);
        assert_eq!(preemphasis, Some(1));
        assert_eq!(count, 1, "historical row must survive migration recovery");
        assert!(
            columns
                .iter()
                .any(|(name, declared_type)| name == "preemphasis_corr" && declared_type == "REAL"),
            "v10 must add the independently missing second column"
        );
    }

    #[test]
    fn rejects_database_newer_than_current_version_without_downgrade() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        let newer_version = CURRENT_VERSION + 1;
        {
            let conn = Connection::open(&path).expect("create newer database");
            conn.pragma_update(None, "user_version", newer_version)
                .expect("stamp newer database");
        }

        let error = Database::open_path(&path)
            .err()
            .expect("newer schema must be rejected");
        assert_eq!(
            error,
            format!(
                "database schema is newer than this build (found {}, supports {})",
                newer_version, CURRENT_VERSION
            )
        );

        let conn = Connection::open(&path).expect("reopen newer database directly");
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read newer version after rejected open");
        assert_eq!(version, newer_version, "rejected open must not downgrade schema");
    }

    #[test]
    fn current_version_open_is_a_migration_noop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create current-version database");
            conn.pragma_update(None, "user_version", CURRENT_VERSION)
                .expect("stamp current version");
        }

        let db = Database::open_path(&path).expect("open current-version database");
        let version: u32 = db
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read current version");
        let application_table_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .expect("count application tables");

        assert_eq!(version, CURRENT_VERSION);
        assert_eq!(
            application_table_count, 0,
            "current-version open must not execute any migration DDL"
        );
    }

    #[test]
    fn partially_applied_v22_schema_is_reopened_idempotently() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = rusqlite::Connection::open(&path).expect("create partial v22 database");
            create_v23_prerequisite_tables(&conn);
            conn.execute_batch(
                "CREATE TABLE metadata_journal (
                    file_path TEXT PRIMARY KEY,
                    backup_path TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    state TEXT NOT NULL DEFAULT 'prepared'
                 );
                 INSERT INTO metadata_journal (file_path, backup_path, started_at)
                 VALUES ('/music/partial.dsf', '/music/partial.dsf.tonepoet-bak', '2026-07-17T00:00:00Z');
                 PRAGMA user_version = 21;",
            )
            .expect("seed partially applied v22 schema");
        }

        let db = Database::open_path(&path).expect("finish partial v22 migration");
        let entry = db
            .metadata_journal_entry("/music/partial.dsf")
            .expect("read migrated journal")
            .expect("partial journal retained");
        let version: u32 = db
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read migrated version");

        assert_eq!(entry.state, METADATA_STATE_PREPARED);
        assert_eq!(entry.backup_path, "/music/partial.dsf.tonepoet-bak");
        assert_eq!(version, CURRENT_VERSION);
    }

    #[test]
    fn v21_metadata_journal_rows_migrate_to_prepared_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = rusqlite::Connection::open(&path).expect("create v21 database");
            create_v23_prerequisite_tables(&conn);
            conn.execute_batch(
                "CREATE TABLE metadata_journal (
                    file_path TEXT PRIMARY KEY,
                    backup_path TEXT NOT NULL,
                    started_at TEXT NOT NULL
                 );
                 INSERT INTO metadata_journal (file_path, backup_path, started_at)
                 VALUES ('/music/legacy.dsf', '/music/legacy.dsf.tonepoet-bak', '2026-07-17T00:00:00Z');
                 PRAGMA user_version = 21;",
            )
            .expect("seed v21 schema");
        }

        let db = Database::open_path(&path).expect("migrate v21 database");
        let entry = db
            .metadata_journal_entry("/music/legacy.dsf")
            .expect("read migrated journal")
            .expect("legacy journal retained");
        let version: u32 = db
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read migrated version");

        assert_eq!(entry.state, METADATA_STATE_PREPARED);
        assert_eq!(entry.backup_path, "/music/legacy.dsf.tonepoet-bak");
        assert_eq!(version, CURRENT_VERSION);
    }

    #[test]
    fn v23_migrates_queue_order_and_initializes_existing_authority_markers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create v22 database");
            create_v23_prerequisite_tables(&conn);
            conn.execute(
                "INSERT INTO conversion_queue (id, item_json) VALUES ('A', '{}')",
                [],
            )
            .expect("insert queue A");
            conn.execute(
                "INSERT INTO conversion_queue (id, item_json) VALUES ('B', '{}')",
                [],
            )
            .expect("insert queue B");
            conn.execute(
                "INSERT INTO conversion_queue (id, item_json) VALUES ('C', '{}')",
                [],
            )
            .expect("insert queue C");
            conn.execute(
                "INSERT INTO recent_files (file_path, accessed_at, access_count)
                 VALUES ('/music/recent.flac', 123, 1)",
                [],
            )
            .expect("insert recent row");
            conn.pragma_update(None, "user_version", 22)
                .expect("stamp v22");
        }

        let db = Database::open_path(&path).expect("migrate v22 to v23");
        let version: u32 = db
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read migrated version");
        assert_eq!(version, 23);

        let positions = {
            let mut statement = db
                .conn
                .prepare("SELECT id, position FROM conversion_queue ORDER BY position")
                .expect("prepare positions");
            let rows = statement
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
                .expect("query positions");
            rows
                .collect::<Result<Vec<_>, _>>()
                .expect("decode positions")
        };
        assert_eq!(
            positions,
            vec![
                ("A".to_string(), 0),
                ("B".to_string(), 1),
                ("C".to_string(), 2),
            ]
        );
        assert!(db.queue_legacy_import_done().expect("queue marker"));
        assert!(db.recent_legacy_import_done().expect("recent marker"));

        let legacy_queue = queue_item(
            "legacy",
            "/music/legacy.flac",
            crate::convert::ConversionStatus::Paused,
        );
        assert_eq!(
            db.publish_legacy_queue_import(&[legacy_queue], &[])
                .expect("existing queue authority no-op"),
            LegacyImportPublication::AlreadyDone
        );
        let queue_count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM conversion_queue", [], |row| row.get(0))
            .expect("queue count");
        assert_eq!(queue_count, 3, "legacy JSON must not clobber existing rows");

        assert_eq!(
            db.publish_legacy_recent_import(&[("/legacy/recent.flac".to_string(), 999)])
                .expect("existing recent authority no-op"),
            LegacyImportPublication::AlreadyDone
        );
        let recent_count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM recent_files", [], |row| row.get(0))
            .expect("recent count");
        assert_eq!(recent_count, 1);
    }

    #[test]
    fn v23_prunes_preexisting_recent_rows_to_retention_bound() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create v22 database");
            create_v23_prerequisite_tables(&conn);
            for index in 0..75i64 {
                conn.execute(
                    "INSERT INTO recent_files (file_path, accessed_at, access_count)
                     VALUES (?1, ?2, 1)",
                    params![format!("/music/{index:03}.flac"), index],
                )
                .expect("seed recent row");
            }
            conn.pragma_update(None, "user_version", 22)
                .expect("stamp v22");
        }

        let db = Database::open_path(&path).expect("migrate v22 to v23");
        assert!(db.recent_legacy_import_done().expect("recent marker"));
        let retained = db
            .list_recent(RECENT_FILES_RETENTION_LIMIT + 10)
            .expect("list migrated recents");
        assert_eq!(retained.len(), RECENT_FILES_RETENTION_LIMIT);
        assert_eq!(retained.first().map(|row| row.0.as_str()), Some("/music/074.flac"));
        assert_eq!(retained.last().map(|row| row.0.as_str()), Some("/music/025.flac"));
    }

    #[test]
    fn v23_empty_authorities_import_once_then_mark_done() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create v22 database");
            create_v23_prerequisite_tables(&conn);
            conn.pragma_update(None, "user_version", 22)
                .expect("stamp v22");
        }

        let db = Database::open_path(&path).expect("migrate v22 to v23");
        assert!(!db.queue_legacy_import_done().expect("queue marker pending"));
        assert!(!db.recent_legacy_import_done().expect("recent marker pending"));

        let first = queue_item(
            "legacy-a",
            "/offline/Album..Remaster/a.flac",
            crate::convert::ConversionStatus::Paused,
        );
        assert_eq!(
            db.publish_legacy_queue_import(&[first], &[])
                .expect("first queue import"),
            LegacyImportPublication::Imported
        );
        assert!(db.queue_legacy_import_done().expect("queue marker done"));

        let second = queue_item(
            "legacy-b",
            "/music/b.flac",
            crate::convert::ConversionStatus::Paused,
        );
        assert_eq!(
            db.publish_legacy_queue_import(&[second], &[])
                .expect("second queue import"),
            LegacyImportPublication::AlreadyDone
        );
        let queue_ids = db
            .load_queue_items()
            .expect("load imported queue")
            .items
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert_eq!(queue_ids, vec!["legacy-a"]);

        assert_eq!(
            db.publish_legacy_recent_import(&[("/music/a.flac".to_string(), 1)])
                .expect("first recent import"),
            LegacyImportPublication::Imported
        );
        assert!(db.recent_legacy_import_done().expect("recent marker done"));
        assert_eq!(
            db.publish_legacy_recent_import(&[("/music/b.flac".to_string(), 2)])
                .expect("second recent import"),
            LegacyImportPublication::AlreadyDone
        );
        assert_eq!(
            db.list_recent(50).expect("load imported recents"),
            vec![("/music/a.flac".to_string(), 1)]
        );
    }

    #[test]
    fn v23_partial_schema_reentry_is_idempotent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create partial v23 database");
            conn.execute_batch(
                "CREATE TABLE conversion_queue (
                    id        TEXT PRIMARY KEY,
                    item_json TEXT NOT NULL,
                    position  INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE recent_files (
                    file_path    TEXT PRIMARY KEY,
                    accessed_at  INTEGER NOT NULL,
                    access_count INTEGER NOT NULL DEFAULT 1
                 );
                 CREATE TABLE legacy_json_import_state (
                    id                 INTEGER PRIMARY KEY CHECK (id = 1),
                    queue_import_done  INTEGER NOT NULL CHECK (queue_import_done IN (0, 1)),
                    recent_import_done INTEGER NOT NULL CHECK (recent_import_done IN (0, 1))
                 );
                 INSERT INTO conversion_queue (id, item_json, position) VALUES ('A', '{}', 0);
                 INSERT INTO conversion_queue (id, item_json, position) VALUES ('B', '{}', 0);
                 PRAGMA user_version = 22;",
            )
            .expect("seed partial v23 schema");
        }

        let db = Database::open_path(&path).expect("finish idempotent v23");
        let positions = {
            let mut statement = db
                .conn
                .prepare("SELECT id, position FROM conversion_queue ORDER BY position")
                .expect("prepare positions");
            let rows = statement
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
                .expect("query positions");
            rows
                .collect::<Result<Vec<_>, _>>()
                .expect("decode positions")
        };
        assert_eq!(
            positions,
            vec![("A".to_string(), 0), ("B".to_string(), 1)]
        );
        assert!(db.queue_legacy_import_done().expect("queue marker"));
        assert!(!db.recent_legacy_import_done().expect("recent marker"));
    }

    #[test]
    fn failed_v23_rolls_back_schema_and_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create incompatible v22 database");
            conn.execute_batch(
                "CREATE TABLE conversion_queue (
                    id        TEXT PRIMARY KEY,
                    item_json TEXT NOT NULL,
                    position  TEXT NOT NULL DEFAULT '0'
                 );
                 CREATE TABLE recent_files (
                    file_path    TEXT PRIMARY KEY,
                    accessed_at  INTEGER NOT NULL,
                    access_count INTEGER NOT NULL DEFAULT 1
                 );
                 PRAGMA user_version = 22;",
            )
            .expect("seed incompatible v23 column");
        }

        let error = Database::open_path(&path)
            .err()
            .expect("incompatible v23 must fail");
        assert!(error.contains("incompatible existing column conversion_queue.position"));

        let conn = Connection::open(&path).expect("reopen failed migration database");
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read version after failure");
        assert_eq!(version, 22, "failed migration must not advance user_version");
        let marker_table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name = 'legacy_json_import_state'",
                [],
                |row| row.get(0),
            )
            .expect("inspect marker table");
        assert_eq!(marker_table_count, 0, "failed v23 must roll back later DDL");
    }

    #[test]
    fn failed_v23_after_schema_mutation_rolls_back_column_marker_and_version() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let conn = Connection::open(&path).expect("create v22 database");
            create_v23_prerequisite_tables(&conn);
            for index in 0..51i64 {
                conn.execute(
                    "INSERT INTO recent_files (file_path, accessed_at, access_count)
                     VALUES (?1, ?2, 1)",
                    params![format!("/music/{index:03}.flac"), index],
                )
                .expect("seed recent row");
            }
            conn.execute_batch(
                "CREATE TRIGGER reject_v23_recent_prune
                 BEFORE DELETE ON recent_files
                 BEGIN
                     SELECT RAISE(ABORT, 'injected v23 prune failure');
                 END;
                 PRAGMA user_version = 22;",
            )
            .expect("seed failing v23 trigger");
        }

        let error = Database::open_path(&path)
            .err()
            .expect("injected v23 prune failure must abort migration");
        assert!(error.contains("injected v23 prune failure"));

        let conn = Connection::open(&path).expect("reopen failed migration database");
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read version after failure");
        assert_eq!(version, 22);
        let mut columns = conn
            .prepare("PRAGMA table_info(conversion_queue)")
            .expect("inspect queue schema");
        let names = columns
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query queue schema")
            .collect::<Result<Vec<_>, _>>()
            .expect("decode queue schema");
        assert!(
            !names.iter().any(|name| name == "position"),
            "the guarded ADD COLUMN must roll back with the failed migration"
        );
        let marker_table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name = 'legacy_json_import_state'",
                [],
                |row| row.get(0),
            )
            .expect("inspect marker table");
        assert_eq!(marker_table_count, 0);
        let recent_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM recent_files", [], |row| row.get(0))
            .expect("recent rows survive rollback");
        assert_eq!(recent_count, 51);
    }

    #[test]
    fn queue_sync_round_trips_order_and_only_writes_changed_rows() {
        let db = Database::open_memory().expect("database");
        let mut items = vec![
            queue_item("A", "/music/a.flac", crate::convert::ConversionStatus::Paused),
            queue_item("B", "/music/b.flac", crate::convert::ConversionStatus::Paused),
            queue_item("C", "/music/c.flac", crate::convert::ConversionStatus::Paused),
        ];

        let refs = items.iter().collect::<Vec<_>>();
        let first = db.sync_queue(&refs).expect("initial queue sync");
        assert_eq!(first.rows_written, 3);
        assert_eq!(first.rows_deleted, 0);

        let second = db.sync_queue(&refs).expect("unchanged queue sync");
        assert_eq!(second.rows_written, 0, "unchanged rows must not be rewritten");
        assert_eq!(second.rows_deleted, 0);

        for _ in 0..3 {
            let ids = db
                .load_queue_items()
                .expect("ordered queue load")
                .items
                .into_iter()
                .map(|item| item.id)
                .collect::<Vec<_>>();
            assert_eq!(ids, vec!["A", "B", "C"]);
        }

        items[1].status = crate::convert::ConversionStatus::NotConfigured;
        let refs = items.iter().collect::<Vec<_>>();
        let changed = db.sync_queue(&refs).expect("single-row queue sync");
        assert_eq!(changed.rows_written, 1, "only the changed row should be upserted");
        assert_eq!(changed.rows_deleted, 0);
    }

    #[test]
    fn queue_processing_is_persisted_as_interrupted_without_stale_progress() {
        let db = Database::open_memory().expect("database");
        let mut completed = queue_item(
            "A",
            "/music/a.flac",
            crate::convert::ConversionStatus::Completed {
                output_path: PathBuf::from("/out/a.flac"),
                log_path: None,
                warning_count: 0,
            },
        );
        completed.completed_at = Some(chrono::Utc::now());
        let mut processing_b = queue_item(
            "B",
            "/music/b.flac",
            crate::convert::ConversionStatus::Processing {
                progress: 87.0,
                message: Some("encoding".to_string()),
                file_progress: Some((4, 5)),
                phase: None,
                phase_progress: Some(0.75),
            },
        );
        processing_b.started_at = Some(chrono::Utc::now());
        processing_b.active_tracks.insert(
            4,
            crate::convert::queue::TrackProgress {
                track_label: "Track 5".to_string(),
                step_description: "encoding".to_string(),
                progress_fraction: 0.91,
                epoch: 99,
            },
        );
        let mut processing_c = queue_item(
            "C",
            "/music/c.flac",
            crate::convert::ConversionStatus::Processing {
                progress: 12.0,
                message: Some("decoding".to_string()),
                file_progress: None,
                phase: None,
                phase_progress: None,
            },
        );
        processing_c.started_at = Some(chrono::Utc::now());
        let items = vec![completed, processing_b, processing_c];

        db.sync_queue(&items.iter().collect::<Vec<_>>())
            .expect("persist processing snapshot");
        let loaded = db.load_queue_items().expect("reload queue").items;
        assert_eq!(
            loaded.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec!["A", "B", "C"]
        );
        for item in &loaded[1..] {
            assert_eq!(item.status, crate::convert::ConversionStatus::Interrupted);
            assert!(item.can_retry());
            assert!(item.started_at.is_none());
            assert!(item.completed_at.is_none());
            assert!(item.active_tracks.is_empty());
            assert!(item.closed_track_epochs.is_empty());
        }

        let raw_b: String = db
            .conn
            .query_row(
                "SELECT item_json FROM conversion_queue WHERE id = 'B'",
                [],
                |row| row.get(0),
            )
            .expect("read persisted B");
        let value: serde_json::Value = serde_json::from_str(&raw_b).expect("decode persisted B");
        assert_eq!(value.get("status"), Some(&serde_json::Value::String("Interrupted".into())));
        assert!(value.get("active_tracks").is_none());
        assert!(value.get("closed_track_epochs").is_none());
        assert!(value.get("started_at").is_some_and(serde_json::Value::is_null));
    }

    #[test]
    fn queue_cancelled_partial_and_offline_dotdot_names_survive_restart() {
        let db = Database::open_memory().expect("database");
        let cancelled = queue_item(
            "cancelled",
            "/definitely/offline/Artist..Live/track.flac",
            crate::convert::ConversionStatus::Cancelled,
        );
        let partial = queue_item(
            "partial",
            "/definitely/offline/Album...Remaster/disc.flac",
            crate::convert::ConversionStatus::Partial {
                output_path: PathBuf::from("/out/partial.flac"),
                successful: 7,
                failed: 1,
                log_path: PathBuf::from("/out/partial.log"),
            },
        );
        db.sync_queue(&[&cancelled, &partial])
            .expect("persist retryable terminal rows");

        let loaded = db.load_queue_items().expect("reload offline rows").items;
        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().all(|item| item.can_retry()));
        assert_eq!(loaded[0].input_path, cancelled.input_path);
        assert_eq!(loaded[1].input_path, partial.input_path);
        assert_eq!(loaded[0].status, crate::convert::ConversionStatus::Cancelled);
        assert!(matches!(
            &loaded[1].status,
            crate::convert::ConversionStatus::Partial { .. }
        ));
    }

    #[test]
    fn queue_whole_query_failure_is_distinct_from_empty() {
        let db = Database::open_memory().expect("database");
        db.conn
            .execute("DROP TABLE conversion_queue", [])
            .expect("drop queue table");
        let error = db
            .load_queue_items()
            .expect_err("whole-query failure must surface");
        assert!(error.contains("queue load prepare"));
    }

    #[test]
    fn queue_maintenance_transaction_failure_surfaces_without_hiding_loaded_work() {
        let db = Database::open_memory().expect("database");
        let processing = queue_item(
            "processing",
            "/music/processing.flac",
            crate::convert::ConversionStatus::Processing {
                progress: 42.0,
                message: Some("encoding".to_string()),
                file_progress: None,
                phase: None,
                phase_progress: Some(0.42),
            },
        );
        let json = serde_json::to_string(&processing).expect("serialize processing row");
        db.conn
            .execute(
                "INSERT INTO conversion_queue (id, item_json, position) VALUES (?1, ?2, 0)",
                params![processing.id, json],
            )
            .expect("seed legacy processing row");
        db.conn
            .execute_batch(
                "CREATE TRIGGER reject_queue_maintenance
                 BEFORE UPDATE ON conversion_queue
                 BEGIN
                     SELECT RAISE(ABORT, 'injected maintenance failure');
                 END;",
            )
            .expect("install maintenance failure trigger");

        let outcome = db
            .load_queue_items()
            .expect("authoritative rows should still load");
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(
            outcome.items[0].status,
            crate::convert::ConversionStatus::Interrupted
        );
        assert!(outcome.items[0].can_retry());
        assert!(
            outcome
                .degradation
                .as_deref()
                .is_some_and(|message| message.contains("maintenance publication failed")),
            "the failed reconciliation transaction must be surfaced to startup"
        );

        let raw: String = db
            .conn
            .query_row(
                "SELECT item_json FROM conversion_queue WHERE id = 'processing'",
                [],
                |row| row.get(0),
            )
            .expect("read unchanged durable row");
        assert!(raw.contains("Processing"));
    }

    #[test]
    fn retryable_terminal_queue_row_keeps_secret_reference_across_reload() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let db = Database::open_memory().expect("database");
        let mut failed = queue_item(
            "failed-secret",
            "/music/encrypted.7z",
            crate::convert::ConversionStatus::Failed {
                error: "source temporarily unavailable".to_string(),
                log_path: None,
            },
        );
        let reference = crate::secret_store::stable_reference("queue-item", &failed.id)
            .expect("stable reference");
        crate::secret_store::set(&reference, "archive-secret").expect("store secret");
        failed.archive_password_ref = Some(reference.clone());
        failed.archive_password_required = true;

        db.sync_queue(&[&failed]).expect("persist failed item");
        let loaded = db.load_queue_items().expect("reload failed item").items;
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].can_retry());
        assert_eq!(loaded[0].archive_password_ref.as_deref(), Some(reference.as_str()));
        assert_eq!(
            crate::secret_store::get(&reference).expect("secret remains usable"),
            "archive-secret"
        );
    }

    #[test]
    fn failed_queue_publication_never_retires_the_only_secret_reference() {
        let _backend = crate::secret_store::enable_insecure_test_backend();
        let db = Database::open_memory().expect("database");
        let mut item = queue_item(
            "publication-failure",
            "/music/encrypted.7z",
            crate::convert::ConversionStatus::Paused,
        );
        let reference = crate::secret_store::stable_reference("queue-item", &item.id)
            .expect("stable reference");
        crate::secret_store::set(&reference, "archive-secret").expect("store secret");
        item.archive_password_ref = Some(reference.clone());
        item.archive_password_required = true;
        db.sync_queue(&[&item]).expect("persist initial row");

        item.status = crate::convert::ConversionStatus::Completed {
            output_path: PathBuf::from("/out/encrypted.flac"),
            log_path: None,
            warning_count: 0,
        };
        db.conn
            .execute("DROP TABLE conversion_queue", [])
            .expect("force queue publication failure");
        assert!(db.sync_queue(&[&item]).is_err());
        assert_eq!(
            crate::secret_store::get(&reference).expect("failed publication keeps secret"),
            "archive-secret"
        );
    }

    #[test]
    fn recent_rows_are_bounded_newest_first_and_deletions_are_durable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tonepoet.db");
        {
            let db = Database::open_path(&path).expect("database");
            for index in 0..1_000 {
                db.record_recent_at(&format!("/music/{index:04}.flac"), index)
                    .expect("record recent");
            }

            let count: i64 = db
                .conn
                .query_row("SELECT COUNT(*) FROM recent_files", [], |row| row.get(0))
                .expect("recent row count");
            assert_eq!(count, RECENT_FILES_RETENTION_LIMIT as i64);

            let retained = db
                .list_recent(RECENT_FILES_RETENTION_LIMIT)
                .expect("list bounded recents");
            assert_eq!(retained.len(), RECENT_FILES_RETENTION_LIMIT);
            assert_eq!(retained.first().map(|row| row.0.as_str()), Some("/music/0999.flac"));
            assert_eq!(retained.last().map(|row| row.0.as_str()), Some("/music/0950.flac"));

            db.remove_recent("/music/0999.flac")
                .expect("durable recent delete");
        }

        let reopened = Database::open_path(&path).expect("reopen database");
        let retained = reopened.list_recent(RECENT_FILES_RETENTION_LIMIT).expect("reload recents");
        assert!(!retained.iter().any(|row| row.0 == "/music/0999.flac"));
        assert_eq!(retained.len(), RECENT_FILES_RETENTION_LIMIT - 1);
    }

    #[test]
    fn restore_backup_overwrites_existing_destination_and_removes_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        let backup = Database::backup_path_for(&original);
        std::fs::write(&original, b"mutated bytes").expect("write mutated destination");
        std::fs::write(&backup, b"original bytes").expect("write rollback marker");

        Database::restore_backup_for(&original, &backup).expect("restore rollback marker");

        assert_eq!(std::fs::read(&original).expect("read restored destination"), b"original bytes");
        assert!(!backup.exists(), "successful restore must remove rollback marker");
    }

    #[test]
    fn restore_backup_streams_multi_chunk_marker_exactly() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.dsf");
        let backup = Database::backup_path_for(&original);
        let mut expected = vec![0u8; 3 * 1024 * 1024 + 137];
        for (index, byte) in expected.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        std::fs::write(&original, b"mutated destination").expect("write destination");
        std::fs::write(&backup, &expected).expect("write multi-chunk marker");

        Database::restore_backup_for(&original, &backup).expect("stream restore marker");

        assert_eq!(std::fs::read(&original).expect("read restored bytes"), expected);
        assert!(!backup.exists(), "successful streamed restore retires marker");
    }

    #[cfg(unix)]
    #[test]
    fn restore_backup_preserves_symlink_and_target_mode() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.dsf");
        let link = temp.path().join("linked.dsf");
        let backup = Database::backup_path_for(&link);
        std::fs::write(&target, b"mutated target bytes").expect("write target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640))
            .expect("set target mode");
        symlink("target.dsf", &link).expect("create relative symlink");
        std::fs::write(&backup, b"authoritative backup bytes").expect("write marker");

        Database::restore_backup_for(&link, &backup).expect("restore through symlink");

        assert!(std::fs::symlink_metadata(&link)
            .expect("inspect link")
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_link(&link).expect("read link"), std::path::PathBuf::from("target.dsf"));
        assert_eq!(
            std::fs::read(&target).expect("read restored target"),
            b"authoritative backup bytes"
        );
        assert_eq!(
            std::fs::metadata(&target)
                .expect("target metadata")
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert!(!backup.exists());
    }

    #[test]
    fn exclusive_backup_collision_never_claims_or_changes_preexisting_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.dsf");
        let backup = temp.path().join("track.dsf.tonepoet-bak.txn-collision");
        std::fs::write(&original, b"current destination bytes").expect("write destination");
        std::fs::write(&backup, b"foreign authoritative bytes").expect("write marker");

        let error = Database::create_backup(&original, &backup)
            .expect_err("preexisting marker must refuse exclusive backup creation");

        assert_eq!(
            error,
            format!(
                "backup refused: rollback marker '{}' already exists and will not be overwritten",
                backup.display()
            )
        );
        assert_eq!(
            std::fs::read(&backup).expect("read preexisting marker"),
            b"foreign authoritative bytes"
        );
        assert_eq!(
            std::fs::read(&original).expect("read unchanged destination"),
            b"current destination bytes"
        );
    }

    #[test]
    fn existing_rollback_marker_blocks_retry_without_overwriting_authority() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.dsf");
        let backup = Database::backup_path_for(&original);
        std::fs::write(&original, b"partially mutated destination").expect("write destination");
        std::fs::write(&backup, b"authoritative original bytes").expect("write authority");
        let invoked = std::cell::Cell::new(false);

        let error = db
            .atomic_metadata_write(&original, || {
                invoked.set(true);
                Ok(())
            })
            .expect_err("existing rollback authority must block retry");

        assert_eq!(invoked.get(), false);
        assert_eq!(
            error,
            format!(
                "backup refused: rollback marker '{}' already exists and will not be overwritten",
                backup.display()
            )
        );
        assert_eq!(
            std::fs::read(&backup).expect("read retained authority"),
            b"authoritative original bytes"
        );
        assert_eq!(
            std::fs::read(&original).expect("read destination"),
            b"partially mutated destination"
        );
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn failed_rollback_retains_original_marker_and_blocks_retry_without_recopying_mutated_bytes() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.dsf");
        std::fs::write(&original, b"authoritative original bytes").expect("write original");

        let first_error = db
            .atomic_metadata_write(&original, || {
                std::fs::remove_file(&original).map_err(|error| error.to_string())?;
                std::fs::create_dir(&original).map_err(|error| error.to_string())?;
                Err("synthetic writer failure after replacing destination with a directory".to_string())
            })
            .expect_err("rollback over a directory must fail");
        assert!(first_error.contains("write failed AND rollback could not be completed"));

        let path = original.display().to_string();
        let entry = db
            .metadata_journal_entry(&path)
            .expect("read retained journal")
            .expect("prepared journal retained");
        let backup = std::path::PathBuf::from(&entry.backup_path);
        assert_eq!(entry.state, METADATA_STATE_PREPARED);
        assert_eq!(
            std::fs::read(&backup).expect("read retained rollback authority"),
            b"authoritative original bytes"
        );

        let invoked = std::cell::Cell::new(false);
        let retry_error = db
            .atomic_metadata_write(&original, || {
                invoked.set(true);
                Ok(())
            })
            .expect_err("unresolved journal must block retry");

        assert_eq!(invoked.get(), false);
        assert!(retry_error.contains("unresolved prepared journal"));
        assert_eq!(
            std::fs::read(&backup).expect("read authority after blocked retry"),
            b"authoritative original bytes"
        );
    }

    #[test]
    fn duplicate_journal_begin_is_rejected_without_replacing_original_record() {
        let db = Database::open_memory().expect("memory database");
        let file = "/music/album.dsf";
        db.begin_metadata_write(file, "/recovery/original.tonepoet-bak")
            .expect("first journal owner");

        let error = db
            .begin_metadata_write(file, "/recovery/retry.tonepoet-bak")
            .expect_err("second owner must be rejected");
        assert!(error.contains("journal insert refused"));
        let entry = db
            .metadata_journal_entry(file)
            .expect("read journal")
            .expect("journal retained");
        assert_eq!(entry.backup_path, "/recovery/original.tonepoet-bak");
        assert_eq!(entry.state, METADATA_STATE_PREPARED);
    }

    #[test]
    fn committed_cleanup_failure_preserves_new_bytes_during_later_recovery() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        let backup = Database::backup_path_for(&original);
        std::fs::write(&original, b"old bytes").expect("write old destination");

        let error = db
            .atomic_metadata_write(&original, || {
                std::fs::write(&original, b"committed new bytes")
                    .map_err(|error| error.to_string())?;
                let transaction_backup = db
                    .metadata_journal_entry(&original.display().to_string())?
                    .ok_or_else(|| "prepared journal missing inside writer".to_string())?
                    .backup_path;
                let transaction_backup = std::path::PathBuf::from(transaction_backup);
                std::fs::remove_file(&transaction_backup)
                    .map_err(|error| error.to_string())?;
                std::fs::create_dir(&transaction_backup)
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .expect_err("directory marker must force cleanup failure");

        assert!(error.contains("committed, but rollback marker cleanup failed"));
        assert_eq!(
            std::fs::read(&original).expect("read committed destination"),
            b"committed new bytes"
        );
        let entry = db
            .metadata_journal_entry(&original.display().to_string())
            .expect("read journal")
            .expect("committed journal retained");
        assert_eq!(entry.state, METADATA_STATE_COMMITTED);
        let transaction_backup = std::path::PathBuf::from(&entry.backup_path);
        assert!(transaction_backup.is_dir());
        assert!(!backup.exists(), "legacy deterministic marker was never used");

        std::fs::remove_dir(&transaction_backup).expect("remove synthetic cleanup blocker");
        let messages = db.recover_stale_metadata_writes();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("state committed"));
        assert_eq!(
            std::fs::read(&original).expect("read preserved committed destination"),
            b"committed new bytes"
        );
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn committed_journal_delete_failure_preserves_new_bytes_and_recovery_never_restores() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        std::fs::write(&original, b"old bytes").expect("write old destination");
        db.conn
            .execute_batch(
                "CREATE TRIGGER block_metadata_journal_delete
                 BEFORE DELETE ON metadata_journal
                 BEGIN
                   SELECT RAISE(ABORT, 'synthetic journal delete failure');
                 END;",
            )
            .expect("install delete blocker");

        let error = db
            .atomic_metadata_write(&original, || {
                std::fs::write(&original, b"committed new bytes")
                    .map_err(|error| error.to_string())
            })
            .expect_err("journal deletion must be surfaced");

        assert!(error.contains("committed and rollback marker was removed"));
        assert!(error.contains("synthetic journal delete failure"));
        assert_eq!(
            std::fs::read(&original).expect("read committed destination"),
            b"committed new bytes"
        );
        let entry = db
            .metadata_journal_entry(&original.display().to_string())
            .expect("read journal")
            .expect("committed journal retained");
        assert_eq!(entry.state, METADATA_STATE_COMMITTED);
        assert!(!std::path::Path::new(&entry.backup_path).exists());

        db.conn
            .execute_batch("DROP TRIGGER block_metadata_journal_delete;")
            .expect("remove delete blocker");
        let messages = db.recover_stale_metadata_writes();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("state committed"));
        assert_eq!(
            std::fs::read(&original).expect("read preserved committed destination"),
            b"committed new bytes"
        );
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn rolled_back_journal_delete_failure_retains_terminal_state_without_reapplying_backup() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        std::fs::write(&original, b"authoritative original bytes")
            .expect("write original destination");
        db.conn
            .execute_batch(
                "CREATE TRIGGER block_metadata_journal_delete
                 BEFORE DELETE ON metadata_journal
                 BEGIN
                   SELECT RAISE(ABORT, 'synthetic journal delete failure');
                 END;",
            )
            .expect("install delete blocker");

        let error = db
            .atomic_metadata_write(&original, || {
                std::fs::write(&original, b"partially mutated bytes")
                    .map_err(|error| error.to_string())?;
                Err("synthetic writer failure".to_string())
            })
            .expect_err("rollback journal deletion must be surfaced");

        assert!(error.contains("original bytes were restored and rollback marker removed"));
        assert!(error.contains("synthetic journal delete failure"));
        assert_eq!(
            std::fs::read(&original).expect("read restored destination"),
            b"authoritative original bytes"
        );
        let entry = db
            .metadata_journal_entry(&original.display().to_string())
            .expect("read journal")
            .expect("rolled-back journal retained");
        assert_eq!(entry.state, METADATA_STATE_ROLLED_BACK);
        assert!(!std::path::Path::new(&entry.backup_path).exists());

        db.conn
            .execute_batch("DROP TRIGGER block_metadata_journal_delete;")
            .expect("remove delete blocker");
        let messages = db.recover_stale_metadata_writes();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("state rolled_back"));
        assert_eq!(
            std::fs::read(&original).expect("read preserved restored destination"),
            b"authoritative original bytes"
        );
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn allocating_recovery_retires_marker_without_replacing_destination() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        let backup = temp.path().join("track.opus.tonepoet-bak.txn-test");
        std::fs::write(&original, b"current destination bytes").expect("write destination");
        std::fs::write(&backup, b"partial allocation bytes").expect("write partial marker");
        db.begin_metadata_write_with_state(
            &original.display().to_string(),
            &backup.display().to_string(),
            METADATA_STATE_ALLOCATING,
        )
        .expect("begin allocating journal");

        let messages = db.recover_stale_metadata_writes();

        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with("Retired incomplete metadata backup allocation"));
        assert_eq!(
            std::fs::read(&original).expect("read preserved destination"),
            b"current destination bytes"
        );
        assert!(!backup.exists());
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn rolled_back_terminal_recovery_never_reapplies_stale_backup() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        let backup = Database::backup_path_for(&original);
        std::fs::write(&original, b"already restored original").expect("write restored bytes");
        std::fs::write(&backup, b"stale marker bytes").expect("write stale marker");
        let path = original.display().to_string();
        db.begin_metadata_write(&path, &backup.display().to_string())
            .expect("begin journal");
        db.set_metadata_write_state(&path, METADATA_STATE_ROLLED_BACK)
            .expect("record rollback completion");

        let messages = db.recover_stale_metadata_writes();

        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("state rolled_back"));
        assert_eq!(
            std::fs::read(&original).expect("read preserved restored destination"),
            b"already restored original"
        );
        assert!(!backup.exists());
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn atomic_metadata_write_restores_exact_bytes_after_mutating_failure() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        std::fs::write(&original, b"original bytes").expect("write original");

        let result = db.atomic_metadata_write(&original, || {
            std::fs::write(&original, b"partially mutated bytes")
                .map_err(|error| error.to_string())?;
            Err("synthetic writer failure".to_string())
        });

        assert_eq!(
            result.expect_err("writer failure must propagate"),
            "write failed (rolled back): synthetic writer failure"
        );
        assert_eq!(std::fs::read(&original).expect("read restored original"), b"original bytes");
        assert!(!Database::backup_path_for(&original).exists());
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn durability_sync_failure_restores_exact_bytes_before_commit() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        std::fs::write(&original, b"durable original bytes").expect("write original");

        let result = db.atomic_metadata_write_with_durability(
            &original,
            || {
                std::fs::write(&original, b"unsynced replacement bytes")
                    .map_err(|error| error.to_string())
            },
            |_| Err("synthetic destination sync failure".to_string()),
        );

        assert_eq!(
            result.expect_err("sync failure must roll back before commit"),
            "write failed (rolled back): metadata durability sync failed: synthetic destination sync failure"
        );
        assert_eq!(
            std::fs::read(&original).expect("read restored destination"),
            b"durable original bytes"
        );
        assert!(!Database::backup_path_for(&original).exists());
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn stale_metadata_recovery_restores_exact_bytes_and_clears_authority() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        let backup = Database::backup_path_for(&original);
        std::fs::write(&original, b"partially mutated bytes").expect("write destination");
        std::fs::write(&backup, b"original bytes").expect("write rollback marker");
        db.begin_metadata_write(&original.display().to_string(), &backup.display().to_string())
            .expect("begin journal");

        let messages = db.recover_stale_metadata_writes();

        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with(&format!("Recovered: {} (write started ", original.display())));
        assert_eq!(std::fs::read(&original).expect("read restored original"), b"original bytes");
        assert!(!backup.exists());
        assert!(db.stale_metadata_writes().expect("read journal").is_empty());
    }

    #[test]
    fn stale_metadata_recovery_failure_retains_journal_authority() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        let backup = Database::backup_path_for(&original);
        std::fs::write(&original, b"partially mutated bytes").expect("write destination");
        std::fs::create_dir(&backup).expect("directory rollback marker forces copy failure");
        let original_string = original.display().to_string();
        let backup_string = backup.display().to_string();
        db.begin_metadata_write(&original_string, &backup_string)
            .expect("begin journal");

        let messages = db.recover_stale_metadata_writes();

        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with(&format!(
            "RECOVERY FAILED for {}: restore '{}' from rollback marker '{}'",
            original.display(),
            original.display(),
            backup.display()
        )));
        assert_eq!(
            std::fs::read(&original).expect("destination unchanged"),
            b"partially mutated bytes"
        );
        assert!(backup.is_dir());
        let retained = db
            .stale_metadata_writes()
            .expect("read retained journal");
        assert_eq!(retained.len(), 1);
        assert_eq!(&retained[0].0, &original_string);
        assert_eq!(&retained[0].1, &backup_string);
    }

    #[test]
    fn stale_metadata_recovery_missing_marker_reports_failure_and_retains_blocking_authority() {
        let db = Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let original = temp.path().join("track.opus");
        let backup = Database::backup_path_for(&original);
        std::fs::write(&original, b"partially mutated bytes").expect("write destination");
        db.begin_metadata_write(&original.display().to_string(), &backup.display().to_string())
            .expect("begin journal");

        let messages = db.recover_stale_metadata_writes();

        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with(&format!(
            "RECOVERY FAILED for {}: rollback marker is missing (write started ",
            original.display()
        )));
        assert!(messages[0].contains("prepared journal remains unresolved and blocks retries"));
        assert_eq!(
            std::fs::read(&original).expect("destination unchanged"),
            b"partially mutated bytes"
        );
        let retained = db.stale_metadata_writes().expect("read retained journal");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].0, original.display().to_string());
        assert_eq!(retained[0].1, backup.display().to_string());
    }

    #[test]
    fn ctdb_parity_cache_round_trip() {
        let db = Database::open_memory().unwrap();
        let parity: Vec<Vec<u16>> = (0..10)
            .map(|j| {
                (0..16)
                    .map(|i| (j as u16 * 16 + i as u16).wrapping_mul(0x1357))
                    .collect()
            })
            .collect();

        // Miss before store
        assert!(db.get_cached_ctdb_parity("test_key", 16).is_none());

        // Store + hit
        db.store_ctdb_parity("test_key", 16, &parity).unwrap();
        let got = db.get_cached_ctdb_parity("test_key", 16).unwrap();
        assert_eq!(got, parity);

        // Different npar = miss (composite primary key)
        assert!(db.get_cached_ctdb_parity("test_key", 8).is_none());

        // Different cache_key = miss
        assert!(db.get_cached_ctdb_parity("other_key", 16).is_none());

        // Re-store same key = INSERT OR REPLACE (idempotent)
        db.store_ctdb_parity("test_key", 16, &parity).unwrap();
        let again = db.get_cached_ctdb_parity("test_key", 16).unwrap();
        assert_eq!(again, parity);
    }

    #[test]
    fn ctdb_parity_cache_lru_promotes_on_hit() {
        // The promote-on-hit invariant: a `get` updates the row's
        // `accessed_at`, so a row that's read just before eviction
        // must survive even if it was the oldest when inserted.
        let db = Database::open_memory().unwrap();
        let parity: Vec<Vec<u16>> = (0..2).map(|j| vec![j as u16; 4]).collect();

        // Insert one key first, so it has the oldest accessed_at.
        db.store_ctdb_parity("first", 4, &parity).unwrap();

        // Fill up to (and not over) the eviction threshold. After this
        // loop, count = threshold, and "first" still has the oldest
        // accessed_at of all rows.
        let threshold = CTDB_PARITY_CACHE_EVICT_THRESHOLD;
        for i in 0..(threshold - 1) {
            let key = format!("key_{:04}", i);
            db.store_ctdb_parity(&key, 4, &parity).unwrap();
        }

        let count_before: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM ctdb_parity_cache", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count_before as usize, threshold,
            "expected count to be at threshold before eviction trigger",
        );

        // Hit "first". This must bump its accessed_at to now — making it
        // newer than every key_NNNN that was just inserted.
        assert!(db.get_cached_ctdb_parity("first", 4).is_some());

        // Trigger eviction by inserting one more row.
        db.store_ctdb_parity("trigger", 4, &parity).unwrap();

        // After eviction, count is at target. "first" must survive
        // because the get bumped its accessed_at; if the promote-on-hit
        // logic ever regressed (UPDATE missing, fired with wrong key,
        // etc), "first" would have been the oldest and would be gone.
        assert!(
            db.get_cached_ctdb_parity("first", 4).is_some(),
            "row was evicted despite cache hit promotion — \
             accessed_at UPDATE in get_cached_ctdb_parity may be broken",
        );
        // The trigger row (newest) must survive.
        assert!(
            db.get_cached_ctdb_parity("trigger", 4).is_some(),
            "newest row was evicted",
        );
        // key_0000 was the second-oldest after "first"; with "first"
        // promoted, key_0000 is now the oldest and must be evicted.
        assert!(
            db.get_cached_ctdb_parity("key_0000", 4).is_none(),
            "expected key_0000 to be evicted as oldest after \"first\" promotion",
        );
    }

    #[test]
    fn ctdb_parity_cache_lru_eviction_trims_oldest() {
        let db = Database::open_memory().unwrap();
        // Tiny parity matrices to keep the test cheap; the real eviction
        // logic only cares about row count, not blob size.
        let parity: Vec<Vec<u16>> = (0..2).map(|j| vec![j as u16; 4]).collect();

        // Push past the eviction threshold to force a trim cycle.
        // Each store is also touching accessed_at via INSERT OR REPLACE.
        let n = CTDB_PARITY_CACHE_EVICT_THRESHOLD + 5;
        for i in 0..n {
            let key = format!("key_{:04}", i);
            db.store_ctdb_parity(&key, 4, &parity).unwrap();
        }

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM ctdb_parity_cache", [], |row| {
                row.get(0)
            })
            .unwrap();

        // Eviction trims to TARGET when the count exceeds THRESHOLD.
        // Between trims the count can climb back up toward THRESHOLD, so
        // the steady-state invariant is `count <= THRESHOLD` — strictly
        // less than `THRESHOLD + 1`.
        assert!(
            (count as usize) <= CTDB_PARITY_CACHE_EVICT_THRESHOLD,
            "expected count <= threshold ({}), got {}",
            CTDB_PARITY_CACHE_EVICT_THRESHOLD,
            count
        );
        // Also assert that an eviction has actually fired (we pushed past
        // the threshold and then some) — count must be below where we'd
        // have landed without any eviction.
        let n_inserted = CTDB_PARITY_CACHE_EVICT_THRESHOLD + 5;
        assert!(
            (count as usize) < n_inserted,
            "expected count < {} (no eviction would have fired), got {}",
            n_inserted,
            count
        );

        // The most recent insertions must still be present (LRU keeps
        // newest by accessed_at).
        for i in (n - 5)..n {
            let key = format!("key_{:04}", i);
            assert!(
                db.get_cached_ctdb_parity(&key, 4).is_some(),
                "expected recent key {} to survive eviction",
                key
            );
        }
    }

    #[test]
    fn metadata_journal_round_trip() {
        let db = Database::open_memory().unwrap();
        db.begin_metadata_write("/music/song.mp3", "/music/song.mp3.tonepoet-bak")
            .unwrap();

        let stale = db.stale_metadata_writes().unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].0, "/music/song.mp3");

        db.complete_metadata_write("/music/song.mp3").unwrap();
        let stale = db.stale_metadata_writes().unwrap();
        assert!(stale.is_empty());
    }

    #[test]
    fn journal_free_metadata_guard_is_read_only_and_refuses_both_authorities() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("song.mp3");
        std::fs::write(&target, b"carrier").expect("write target");
        let backup = Database::backup_path_for(&target);
        let target_string = target.display().to_string();
        let backup_string = backup.display().to_string();
        let db = Database::open_memory().expect("database");

        db.begin_metadata_write(&target_string, &backup_string)
            .expect("seed journal authority");
        let journal_error = db
            .assert_metadata_write_unarmed(&target)
            .expect_err("armed journal must refuse standard replacement");
        assert!(journal_error.contains("unresolved prepared journal"));
        assert_eq!(db.stale_metadata_writes().expect("read journal").len(), 1);

        db.complete_metadata_write(&target_string)
            .expect("retire journal authority");
        std::fs::write(&backup, b"stale rollback authority").expect("write stale backup");
        let backup_error = db
            .assert_metadata_write_unarmed(&target)
            .expect_err("stale legacy backup must refuse standard replacement");
        assert!(backup_error.contains("stale rollback marker"));
        assert_eq!(
            std::fs::read(&backup).expect("marker retained"),
            b"stale rollback authority",
            "read-only guard must not retire recovery authority",
        );
    }

    #[test]
    fn pending_archive_recovery_round_trip_uses_existing_staging() {
        let db = Database::open_memory().unwrap();
        // Use a path that won't be caught by the nix-shell test-artifact
        // filter. The filter checks for "nix-shell." in path components,
        // so we create a dedicated dir under /tmp with a non-matching name.
        let base = std::path::PathBuf::from("/tmp/tonepoet-db-roundtrip-test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let archive = base.join("album.zip");
        let staging = base.join("tonepoet-archive-rename-test");
        std::fs::write(&archive, b"archive bytes").unwrap();
        std::fs::create_dir_all(&staging).unwrap();

        db.upsert_pending_archive_session(&archive, &staging, 0, 0, 13, "[]")
            .unwrap();
        let sessions = db.recover_pending_archive_sessions_at_startup().unwrap();

        // Clean up before asserting so the dir doesn't linger on failure.
        let _ = std::fs::remove_dir_all(&base);

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].archive_path, archive);
        assert_eq!(sessions[0].staging_dir, staging);
    }

    #[test]
    fn pending_archive_recovery_prunes_nix_shell_test_artifacts() {
        let db = Database::open_memory().unwrap();
        let root = std::env::temp_dir().join(format!(
            "nix-shell.{}",
            uuid::Uuid::new_v4()
        ));
        let archive_parent = root.join(format!(".tmp{}", uuid::Uuid::new_v4()));
        let archive = archive_parent.join("album.zip");
        let staging = root.join(format!(
            "tonepoet-archive-rename-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&archive_parent).unwrap();
        std::fs::write(&archive, b"test fixture").unwrap();
        std::fs::create_dir_all(&staging).unwrap();

        db.upsert_pending_archive_session(&archive, &staging, 0, 0, 12, "[]")
            .unwrap();
        assert_eq!(db.pending_archive_session_count_for_tests().unwrap(), 1);

        let sessions = db.recover_pending_archive_sessions_at_startup().unwrap();
        assert!(sessions.is_empty());
        assert_eq!(db.pending_archive_session_count_for_tests().unwrap(), 0);
        assert!(!staging.exists(), "test artifact staging dir should be cleaned");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn directory_summary_cache_round_trip_and_rejects_stale_identity() {
        use crate::tui::browse::{
            DirStats, DirectorySummaryCacheEntry, DirectorySummaryFacts, DirectorySummaryScope,
            FolderAudioSummary, FolderClassificationKind, FolderContentClassification,
            ProbeCacheIdentity,
        };
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::time::{Duration, SystemTime};

        let db = Database::open_memory().unwrap();
        let path = PathBuf::from("/music/Persistent Album");
        let identity = ProbeCacheIdentity {
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            size: 4096,
        };
        let mut format_counts = BTreeMap::new();
        format_counts.insert("FLAC".to_string(), 2usize);
        let audio = FolderAudioSummary {
            track_count: 2,
            format_counts,
            file_paths: vec![path.join("01.flac"), path.join("02.flac")],
        };
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Album,
            identity,
            audio,
            units: Vec::new(),
            unit_count: 1,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: None,
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
        };
        let entry = DirectorySummaryCacheEntry {
            identity,
            facts: DirectorySummaryFacts {
                classification: Some(Arc::new(classification)),
                classification_scope: Some(DirectorySummaryScope::Immediate),
                stats: Some(Arc::new(DirStats {
                    folder_count: 0,
                    file_count: 2,
                    audio_count: 2,
                    audio_size: 12345,
                    total_size: 12345,
                })),
                stats_scope: Some(DirectorySummaryScope::RecursiveBestEffort),
            },
        };

        db.store_directory_summary(&path, &entry).unwrap();
        let cached = db
            .get_cached_directory_summary(&path, identity)
            .expect("fresh identity should hit");
        assert_eq!(
            cached.facts.classification.as_ref().map(|classification| classification.kind),
            Some(FolderClassificationKind::Album),
        );
        assert_eq!(cached.facts.classification_scope, Some(DirectorySummaryScope::Immediate));
        assert_eq!(
            cached.facts.stats_scope,
            Some(DirectorySummaryScope::RecursiveBestEffort),
        );
        assert_eq!(cached.facts.stats.as_ref().map(|stats| stats.audio_count), Some(2));

        let changed_size = ProbeCacheIdentity { size: identity.size + 1, ..identity };
        assert!(db.get_cached_directory_summary(&path, changed_size).is_none());

        let changed_mtime = ProbeCacheIdentity {
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_001)),
            ..identity
        };
        assert!(db.get_cached_directory_summary(&path, changed_mtime).is_none());
    }

    #[test]
    fn probe_cache_round_trip() {
        let db = Database::open_memory().unwrap();

        let row = CachedProbeRow {
            format_name: Some("flac".into()),
            codec: Some("flac".into()),
            sample_rate: Some(44100),
            channels: Some(2),
            title: Some("Test Song".into()),
            artist: Some("Test Artist".into()),
            ..Default::default()
        };

        db.store_probe("/music/song.mp3", 1000, 5000000, &row)
            .unwrap();

        // Hit: same mtime + size.
        let cached = db.get_cached_probe("/music/song.mp3", 1000, 5000000);
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().title, Some("Test Song".into()));

        // Miss: different mtime.
        let cached = db.get_cached_probe("/music/song.mp3", 2000, 5000000);
        assert!(cached.is_none());

        // Invalidate.
        db.invalidate_probe("/music/song.mp3").unwrap();
        let cached = db.get_cached_probe("/music/song.mp3", 1000, 5000000);
        assert!(cached.is_none());
    }

    #[test]
    fn batch_probe_cache_filters_stale_mtime_and_size_rows() {
        let db = Database::open_memory().unwrap();
        let row = CachedProbeRow {
            format_name: Some("flac".into()),
            codec: Some("flac".into()),
            sample_rate: Some(44100),
            channels: Some(2),
            title: Some("Batch Song".into()),
            ..Default::default()
        };
        db.store_probe("/music/batch.flac", 1000, 5000000, &row)
            .unwrap();

        let stale_mtime = db.get_cached_probes_for_files(&[(
            "/music/batch.flac".to_string(),
            1001,
            5000000,
        )]);
        assert!(stale_mtime.is_empty());

        let stale_size = db.get_cached_probes_for_files(&[(
            "/music/batch.flac".to_string(),
            1000,
            5000001,
        )]);
        assert!(stale_size.is_empty());

        let fresh = db.get_cached_probes_for_files(&[(
            "/music/batch.flac".to_string(),
            1000,
            5000000,
        )]);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].1.title.as_deref(), Some("Batch Song"));
    }

    #[test]
    fn metadata_analysis_facts_store_preserves_unattempted_detectors_for_same_file() {
        use crate::tui::app::MetadataAnalysisFacts;
        use crate::tui::preemphasis::PreemphasisConfidence;

        let db = Database::open_memory().unwrap();
        let path = "/music/song.mp3";
        let complete = MetadataAnalysisFacts {
            hdcd_detected: Some(true),
            hdcd_detail: Some("HDCD detected".into()),
            preemphasis: Some(PreemphasisConfidence::Detected),
            preemphasis_detail: Some("CUE FLAGS PRE".into()),
        };
        db.store_metadata_analysis_facts(path, 1000, 5000000, &complete)
            .unwrap();

        let pre_only = MetadataAnalysisFacts {
            hdcd_detected: None,
            hdcd_detail: None,
            preemphasis: Some(PreemphasisConfidence::NotDetected),
            preemphasis_detail: Some("no PRE tag, CUE flag, or catalog match".into()),
        };
        db.store_metadata_analysis_facts(path, 1000, 5000000, &pre_only)
            .unwrap();

        let cached = db
            .get_cached_metadata_analysis_facts(path, 1000, 5000000)
            .expect("same-identity partial analysis remains cached");
        assert_eq!(cached.hdcd_detected, Some(true));
        assert_eq!(cached.hdcd_detail.as_deref(), Some("HDCD detected"));
        assert_eq!(
            cached.preemphasis,
            Some(PreemphasisConfidence::NotDetected),
            "incoming PRE result should still update the PRE columns",
        );
        assert_eq!(
            cached.preemphasis_detail.as_deref(),
            Some("no PRE tag, CUE flag, or catalog match"),
        );

        db.store_metadata_analysis_facts(path, 1001, 5000000, &pre_only)
            .unwrap();
        let changed_identity = db
            .get_cached_metadata_analysis_facts(path, 1001, 5000000)
            .expect("new-identity partial analysis remains cached");
        assert_eq!(
            changed_identity.hdcd_detected, None,
            "a different file identity must not inherit HDCD facts from old bytes",
        );
        assert_eq!(
            changed_identity.preemphasis,
            Some(PreemphasisConfidence::NotDetected),
        );
    }

    #[test]
    fn analysis_cache_version_rejects_pre_authoritative_catalog_results() {
        use crate::tui::preemphasis::PreemphasisConfidence;

        let db = Database::open_memory().unwrap();
        let path = "/music/false-series.flac";
        db.conn
            .execute(
                "INSERT INTO analysis_cache (
                    file_path, file_mtime, file_size, algo_version,
                    preemphasis, preemphasis_detail, analyzed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    path,
                    1000i64,
                    5_000_000i64,
                    24i32,
                    3i32,
                    "catalog match: 35DP-999",
                    "2026-08-05T00:00:00Z",
                ],
            )
            .unwrap();

        assert!(
            db.get_cached_metadata_analysis_facts(path, 1000, 5_000_000)
                .is_none(),
            "version-24 catalog candidates must miss after authoritative matching replaced series inference",
        );
        assert!(db.get_cached_analysis(path, 1000, 5_000_000).is_none());

        let current = crate::tui::app::MetadataAnalysisFacts {
            preemphasis: Some(PreemphasisConfidence::Possible),
            preemphasis_detail: Some(
                "possible catalog evidence: folder exact 35DP-150".to_string(),
            ),
            hdcd_detected: None,
            hdcd_detail: None,
        };
        db.store_metadata_analysis_facts(path, 1000, 5_000_000, &current)
            .unwrap();

        let cached = db
            .get_cached_metadata_analysis_facts(path, 1000, 5_000_000)
            .expect("current-version result should hit");
        assert_eq!(cached.preemphasis, Some(PreemphasisConfidence::Possible));
        assert_eq!(
            cached.preemphasis_detail.as_deref(),
            Some("possible catalog evidence: folder exact 35DP-150"),
        );
    }

    #[test]
    fn recent_same_second_reaccess_preserves_true_recency_order() {
        let db = Database::open_memory().expect("database");
        db.record_recent_at("/music/a.flac", 1000)
            .expect("record a");
        db.record_recent_at("/music/b.flac", 1000)
            .expect("record b");
        db.record_recent_at("/music/a.flac", 1000)
            .expect("re-record a");

        let recent = db.list_recent(10).expect("list recents");
        assert_eq!(
            recent.into_iter().map(|row| row.0).collect::<Vec<_>>(),
            vec!["/music/a.flac".to_string(), "/music/b.flac".to_string()]
        );
        let access_count: i64 = db
            .conn
            .query_row(
                "SELECT access_count FROM recent_files WHERE file_path = '/music/a.flac'",
                [],
                |row| row.get(0),
            )
            .expect("read access count");
        assert_eq!(access_count, 2);
    }

    #[test]
    fn recent_files_round_trip() {
        let db = Database::open_memory().unwrap();
        // Insert with explicit timestamps to avoid same-second ordering issues.
        db.conn
            .execute(
                "INSERT INTO recent_files (file_path, accessed_at) VALUES (?1, ?2)",
                params!["/music/a.flac", 1000i64],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO recent_files (file_path, accessed_at) VALUES (?1, ?2)",
                params!["/music/b.flac", 2000i64],
            )
            .unwrap();

        let recent = db.list_recent(10).unwrap();
        assert_eq!(recent.len(), 2);
        // Most recent first.
        assert_eq!(recent[0].0, "/music/b.flac");
        assert_eq!(recent[1].0, "/music/a.flac");
    }

    #[test]
    fn replace_bookmarks_transactional_preserves_exact_order() {
        let db = Database::open_memory().unwrap();
        db.add_bookmark("stale", "/stale").unwrap();
        db.replace_bookmarks_transactional(&[
            ("Second".to_string(), "/two".to_string()),
            ("First".to_string(), "/one".to_string()),
        ])
        .unwrap();

        let rows = db.list_bookmarks().unwrap();
        assert_eq!(
            rows.into_iter()
                .map(|(_, name, path)| (name, path))
                .collect::<Vec<_>>(),
            vec![
                ("Second".to_string(), "/two".to_string()),
                ("First".to_string(), "/one".to_string()),
            ]
        );
    }

    #[test]
    fn replace_bookmarks_transactional_rolls_back_on_insert_failure() {
        let db = Database::open_memory().unwrap();
        db.replace_bookmarks_transactional(&[
            ("Original A".to_string(), "/a".to_string()),
            ("Original B".to_string(), "/b".to_string()),
        ])
        .unwrap();
        db.conn
            .execute_batch(
                "CREATE TRIGGER reject_injected_bookmark
                 BEFORE INSERT ON bookmarks
                 WHEN NEW.name = 'FAIL'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected bookmark mirror failure');
                 END;",
            )
            .unwrap();

        let result = db.replace_bookmarks_transactional(&[
            ("Replacement".to_string(), "/replacement".to_string()),
            ("FAIL".to_string(), "/fail".to_string()),
        ]);
        assert!(result.is_err());

        let rows = db.list_bookmarks().unwrap();
        assert_eq!(
            rows.into_iter()
                .map(|(_, name, path)| (name, path))
                .collect::<Vec<_>>(),
            vec![
                ("Original A".to_string(), "/a".to_string()),
                ("Original B".to_string(), "/b".to_string()),
            ]
        );
    }

    #[test]
    fn bookmarks_round_trip() {
        let db = Database::open_memory().unwrap();
        db.add_bookmark("Music", "/home/user/music").unwrap();
        db.add_bookmark("Downloads", "/home/user/downloads")
            .unwrap();

        let bm = db.list_bookmarks().unwrap();
        assert_eq!(bm.len(), 2);
        assert_eq!(bm[0].1, "Music");
        assert_eq!(bm[1].1, "Downloads");

        // Remove first.
        db.remove_bookmark(bm[0].0).unwrap();
        let bm = db.list_bookmarks().unwrap();
        assert_eq!(bm.len(), 1);
        assert_eq!(bm[0].1, "Downloads");
    }
}
