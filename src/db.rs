//! SQLite database: core persistence layer for tonepoet.
//!
//! Database at `~/.local/share/tonepoet/tonepoet.db` (XDG_DATA_HOME).
//! WAL mode enabled for crash safety. Schema versioned via PRAGMA
//! user_version with forward migrations on open.

use std::path::PathBuf;
use rusqlite::{Connection, params};

/// Schema version — bump when adding migrations.
const CURRENT_VERSION: u32 = 1;

/// Core database wrapper. Owns a single SQLite connection.
pub struct Database {
    conn: Connection,
}

/// Return the database file path.
pub fn db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tonepoet")
        .join("tonepoet.db")
}

impl Database {
    /// Open (or create) the database, run migrations, enable WAL.
    pub fn open() -> Result<Self, String> {
        let path = db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create DB directory: {}", e))?;
        }

        let conn = Connection::open(&path)
            .map_err(|e| format!("failed to open database: {}", e))?;

        // WAL mode for crash safety + concurrent reads.
        conn.execute_batch("PRAGMA journal_mode = WAL;")
            .map_err(|e| format!("WAL pragma failed: {}", e))?;

        // Foreign keys (for future use).
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|e| format!("foreign_keys pragma failed: {}", e))?;

        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory database (for tests and fallback).
    pub fn open_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory()
            .map_err(|e| format!("failed to open in-memory DB: {}", e))?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")
            .map_err(|e| format!("WAL pragma failed: {}", e))?;
        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Run forward migrations up to CURRENT_VERSION.
    fn migrate(&mut self) -> Result<(), String> {
        let version: u32 = self.conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|e| format!("read user_version: {}", e))?;

        if version < 1 {
            self.migrate_v1()?;
        }

        // Future: if version < 2 { self.migrate_v2()?; }

        self.conn
            .pragma_update(None, "user_version", CURRENT_VERSION)
            .map_err(|e| format!("set user_version: {}", e))?;

        Ok(())
    }

    /// v1: metadata journal, probe cache, recent files, bookmarks.
    fn migrate_v1(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
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
        ").map_err(|e| format!("v1 migration failed: {}", e))?;

        Ok(())
    }

    // ── Metadata journal ─────────────────────────────────────────

    /// Record an in-flight metadata write. Called BEFORE the actual write.
    pub fn begin_metadata_write(&self, file_path: &str, backup_path: &str) -> Result<(), String> {
        self.conn.execute(
            "INSERT OR REPLACE INTO metadata_journal (file_path, backup_path, started_at)
             VALUES (?1, ?2, ?3)",
            params![file_path, backup_path, chrono::Utc::now().to_rfc3339()],
        ).map_err(|e| format!("journal insert: {}", e))?;
        Ok(())
    }

    /// Remove the journal entry after a successful write.
    pub fn complete_metadata_write(&self, file_path: &str) -> Result<(), String> {
        self.conn.execute(
            "DELETE FROM metadata_journal WHERE file_path = ?1",
            params![file_path],
        ).map_err(|e| format!("journal delete: {}", e))?;
        Ok(())
    }

    /// Get all stale journal entries (for crash recovery on startup).
    pub fn stale_metadata_writes(&self) -> Result<Vec<(String, String, String)>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT file_path, backup_path, started_at FROM metadata_journal"
        ).map_err(|e| format!("journal query: {}", e))?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        }).map_err(|e| format!("journal query: {}", e))?;

        let mut entries = Vec::new();
        for row in rows {
            if let Ok(entry) = row {
                entries.push(entry);
            }
        }
        Ok(entries)
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
        self.conn.query_row(
            "SELECT * FROM probe_cache WHERE file_path = ?1
             AND file_mtime = ?2 AND file_size = ?3",
            params![file_path, current_mtime, current_size as i64],
            |row| Ok(CachedProbeRow {
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
            }),
        ).ok()
    }

    /// Store a probe result in the cache (upsert).
    pub fn store_probe(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
        row: &CachedProbeRow,
    ) -> Result<(), String> {
        self.conn.execute(
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
                file_path, mtime, size as i64,
                row.format_name, row.codec, row.bit_depth, row.sample_rate, row.channels,
                row.channel_layout, row.duration_secs,
                row.title, row.artist, row.album, row.genre, row.year,
                row.track_number, row.catalog_number,
                row.rg_track_gain, row.rg_track_peak, row.rg_album_gain, row.rg_album_peak,
                row.r128_track_gain, row.r128_album_gain,
                chrono::Utc::now().to_rfc3339(),
            ],
        ).map_err(|e| format!("probe cache store: {}", e))?;
        Ok(())
    }

    /// Invalidate cache for a specific file (after metadata edit).
    pub fn invalidate_probe(&self, file_path: &str) -> Result<(), String> {
        self.conn.execute(
            "DELETE FROM probe_cache WHERE file_path = ?1",
            params![file_path],
        ).map_err(|e| format!("probe cache invalidate: {}", e))?;
        Ok(())
    }

    // ── Atomic metadata write ──────────────────────────────────

    /// Perform an atomic metadata write with hardlink backup + journal.
    ///
    /// 1. Creates a hardlink backup (copy fallback for cross-fs)
    /// 2. Records the in-flight write in the journal table
    /// 3. Calls the provided write function
    /// 4. On success: removes journal entry + backup
    /// 5. On failure: restores from backup (instant rollback)
    pub fn atomic_metadata_write<F>(
        &self,
        file_path: &std::path::Path,
        write_fn: F,
    ) -> Result<(), String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        let path_str = file_path.display().to_string();
        let backup = Self::backup_path(file_path);
        let backup_str = backup.display().to_string();

        // Step 1: create backup.
        Self::create_backup(file_path, &backup)?;

        // Step 2: record in journal.
        if let Err(e) = self.begin_metadata_write(&path_str, &backup_str) {
            let _ = std::fs::remove_file(&backup);
            return Err(format!("journal error (write aborted): {}", e));
        }

        // Step 3: execute the write.
        let result = write_fn();

        // Step 4/5: cleanup.
        match result {
            Ok(()) => {
                let _ = self.complete_metadata_write(&path_str);
                let _ = std::fs::remove_file(&backup);
                Ok(())
            }
            Err(e) => {
                // Rollback: restore from backup.
                if backup.exists() {
                    if let Err(rollback_err) = std::fs::rename(&backup, file_path) {
                        let _ = self.complete_metadata_write(&path_str);
                        return Err(format!(
                            "write failed AND rollback failed ({}: {}). Backup at: {}",
                            e, rollback_err, backup_str
                        ));
                    }
                }
                let _ = self.complete_metadata_write(&path_str);
                Err(format!("write failed (rolled back): {}", e))
            }
        }
    }

    /// Recover from any stale journal entries (crash recovery).
    /// Returns descriptions of recovered files.
    pub fn recover_stale_metadata_writes(&self) -> Vec<String> {
        let entries = match self.stale_metadata_writes() {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };

        let mut messages = Vec::new();
        for (file_path, backup_path, started_at) in &entries {
            let backup = std::path::PathBuf::from(backup_path);
            let original = std::path::PathBuf::from(file_path);

            if backup.exists() {
                match std::fs::rename(&backup, &original) {
                    Ok(()) => {
                        messages.push(format!(
                            "Recovered: {} (write started {})",
                            file_path, started_at
                        ));
                    }
                    Err(e) => {
                        messages.push(format!(
                            "RECOVERY FAILED for {}: {}. Backup at: {}",
                            file_path, e, backup_path
                        ));
                    }
                }
            }
            let _ = self.complete_metadata_write(file_path);
        }
        messages
    }

    /// Backup path: same directory, `.tonepoet-bak` suffix.
    fn backup_path(original: &std::path::Path) -> std::path::PathBuf {
        let mut name = original
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into());
        name.push_str(".tonepoet-bak");
        original.with_file_name(name)
    }

    /// Create a backup by copying the file. We MUST copy, not hardlink:
    /// hardlinks share the same inode, so in-place writes by lofty would
    /// corrupt both the original AND the "backup". A copy has its own
    /// inode and is immune to writes to the original.
    fn create_backup(
        original: &std::path::Path,
        backup: &std::path::Path,
    ) -> Result<(), String> {
        let _ = std::fs::remove_file(backup); // Remove stale backup.
        std::fs::copy(original, backup)
            .map_err(|e| format!("backup failed: {}", e))?;
        Ok(())
    }

    // ── Recent files ─────────────────────────────────────────────

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
        self.conn.execute(
            "INSERT OR REPLACE INTO recent_files (file_path, accessed_at)
             VALUES (?1, ?2)",
            params![file_path, timestamp],
        ).map_err(|e| format!("recent insert: {}", e))?;
        Ok(())
    }

    /// List recent files, most recent first, up to `limit`.
    pub fn list_recent(&self, limit: usize) -> Result<Vec<(String, i64)>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT file_path, accessed_at FROM recent_files
             ORDER BY accessed_at DESC LIMIT ?1"
        ).map_err(|e| format!("recent query: {}", e))?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).map_err(|e| format!("recent query: {}", e))?;

        let mut entries = Vec::new();
        for row in rows {
            if let Ok(entry) = row {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    // ── Bookmarks ────────────────────────────────────────────────

    /// List all bookmarks ordered by position.
    pub fn list_bookmarks(&self) -> Result<Vec<(i64, String, String)>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, path FROM bookmarks ORDER BY position ASC"
        ).map_err(|e| format!("bookmarks query: {}", e))?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        }).map_err(|e| format!("bookmarks query: {}", e))?;

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
        let max_pos: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) FROM bookmarks",
            [],
            |row| row.get(0),
        ).unwrap_or(-1);

        self.conn.execute(
            "INSERT INTO bookmarks (name, path, position) VALUES (?1, ?2, ?3)",
            params![name, path, max_pos + 1],
        ).map_err(|e| format!("bookmark insert: {}", e))?;
        Ok(())
    }

    /// Remove a bookmark by id.
    pub fn remove_bookmark(&self, id: i64) -> Result<(), String> {
        self.conn.execute(
            "DELETE FROM bookmarks WHERE id = ?1",
            params![id],
        ).map_err(|e| format!("bookmark delete: {}", e))?;
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

// ── Conversion helpers ──────────────────────────────────────────

impl CachedProbeRow {
    /// Convert a CachedProbeRow to a CachedInfo (SourceInfo + SourceMetadata).
    /// Returns None if essential fields (format_name, sample_rate, channels) are missing.
    pub fn to_cached_info(&self, file_size: u64) -> Option<crate::tui::browse::CachedInfo> {
        use crate::tui::probe::{SourceInfo, SourceMetadata};
        Some(crate::tui::browse::CachedInfo {
            source: SourceInfo {
                format_name: self.format_name.clone()?,
                codec: self.codec.clone().unwrap_or_default(),
                bit_depth: self.bit_depth,
                sample_rate: self.sample_rate?,
                channels: self.channels?,
                channel_layout: self.channel_layout.clone().unwrap_or_default(),
                duration_secs: self.duration_secs.unwrap_or(0.0),
                file_size,
            },
            metadata: SourceMetadata {
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
            },
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_migrate() {
        let db = Database::open_memory().unwrap();
        let version: u32 = db.conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_VERSION);
    }

    #[test]
    fn metadata_journal_round_trip() {
        let db = Database::open_memory().unwrap();
        db.begin_metadata_write("/music/song.flac", "/music/song.flac.tonepoet-bak").unwrap();

        let stale = db.stale_metadata_writes().unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].0, "/music/song.flac");

        db.complete_metadata_write("/music/song.flac").unwrap();
        let stale = db.stale_metadata_writes().unwrap();
        assert!(stale.is_empty());
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

        db.store_probe("/music/song.flac", 1000, 5000000, &row).unwrap();

        // Hit: same mtime + size.
        let cached = db.get_cached_probe("/music/song.flac", 1000, 5000000);
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().title, Some("Test Song".into()));

        // Miss: different mtime.
        let cached = db.get_cached_probe("/music/song.flac", 2000, 5000000);
        assert!(cached.is_none());

        // Invalidate.
        db.invalidate_probe("/music/song.flac").unwrap();
        let cached = db.get_cached_probe("/music/song.flac", 1000, 5000000);
        assert!(cached.is_none());
    }

    #[test]
    fn recent_files_round_trip() {
        let db = Database::open_memory().unwrap();
        // Insert with explicit timestamps to avoid same-second ordering issues.
        db.conn.execute(
            "INSERT INTO recent_files (file_path, accessed_at) VALUES (?1, ?2)",
            params!["/music/a.flac", 1000i64],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO recent_files (file_path, accessed_at) VALUES (?1, ?2)",
            params!["/music/b.flac", 2000i64],
        ).unwrap();

        let recent = db.list_recent(10).unwrap();
        assert_eq!(recent.len(), 2);
        // Most recent first.
        assert_eq!(recent[0].0, "/music/b.flac");
        assert_eq!(recent[1].0, "/music/a.flac");
    }

    #[test]
    fn bookmarks_round_trip() {
        let db = Database::open_memory().unwrap();
        db.add_bookmark("Music", "/home/user/music").unwrap();
        db.add_bookmark("Downloads", "/home/user/downloads").unwrap();

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
