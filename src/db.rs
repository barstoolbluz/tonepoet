//! SQLite database: core persistence layer for tonepoet.
//!
//! Database at `~/.local/share/tonepoet/tonepoet.db` (XDG_DATA_HOME).
//! WAL mode enabled for crash safety. Schema versioned via PRAGMA
//! user_version with forward migrations on open.

use std::path::PathBuf;
use rusqlite::{Connection, params};

/// Schema version — bump when adding migrations.
const CURRENT_VERSION: u32 = 14;

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
        if version < 2 {
            self.migrate_v2()?;
        }
        if version < 3 {
            self.migrate_v3()?;
        }
        if version < 4 {
            self.migrate_v4()?;
        }
        if version < 5 {
            self.migrate_v5()?;
        }
        if version < 6 {
            self.migrate_v6()?;
        }
        if version < 7 {
            self.migrate_v7()?;
        }
        if version < 8 {
            self.migrate_v8()?;
        }
        if version < 9 {
            self.migrate_v9()?;
        }
        if version < 10 {
            self.migrate_v10()?;
        }
        if version < 11 {
            self.migrate_v11()?;
        }
        if version < 12 {
            self.migrate_v12()?;
        }
        if version < 13 {
            self.migrate_v13()?;
        }
        if version < 14 {
            self.migrate_v14()?;
        }

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

    /// v2: presets table.
    fn migrate_v2(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
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
        ").map_err(|e| format!("v2 migration failed: {}", e))?;
        Ok(())
    }

    /// v3: conversion history table.
    fn migrate_v3(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
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
        ").map_err(|e| format!("v3 migration failed: {}", e))?;
        Ok(())
    }

    /// v4: batch state table for Convert screen recovery.
    fn migrate_v4(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
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
        ").map_err(|e| format!("v4 migration failed: {}", e))?;
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
            &paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
        ).map_err(|e| format!("paths serialize: {}", e))?;

        self.conn.execute(
            "INSERT OR REPLACE INTO batch_state (
                id, paths_json, format, sample_rate, bit_depth,
                dither, replaygain, saved_at
            ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                paths_json, format, sample_rate, bit_depth,
                dither, replaygain, chrono::Utc::now().to_rfc3339(),
            ],
        ).map_err(|e| format!("batch state save: {}", e))?;
        Ok(())
    }

    /// Load the saved batch state, if any. Returns (paths, format, sample_rate,
    /// bit_depth, dither, replaygain).
    pub fn load_batch_state(
        &self,
    ) -> Option<(Vec<std::path::PathBuf>, Option<String>, Option<u32>, Option<String>, Option<String>, Option<String>)> {
        self.conn.query_row(
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
        ).ok().and_then(|(json, format, sr, bd, dither, rg)| {
            let path_strs: Vec<String> = serde_json::from_str(&json).ok()?;
            let paths: Vec<std::path::PathBuf> = path_strs.into_iter()
                .map(std::path::PathBuf::from)
                .filter(|p| p.exists()) // Only restore paths that still exist
                .collect();
            if paths.is_empty() { return None; }
            Some((paths, format, sr, bd, dither, rg))
        })
    }

    /// Clear the saved batch state (after commit or explicit cancel).
    pub fn clear_batch_state(&self) -> Result<(), String> {
        self.conn.execute("DELETE FROM batch_state WHERE id = 1", [])
            .map_err(|e| format!("batch state clear: {}", e))?;
        Ok(())
    }

    /// v5: add access_count to recent_files.
    fn migrate_v5(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
            ALTER TABLE recent_files ADD COLUMN access_count INTEGER NOT NULL DEFAULT 1;
        ").map_err(|e| format!("v5 migration failed: {}", e))?;
        Ok(())
    }

    /// v6: conversion queue table.
    fn migrate_v6(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS conversion_queue (
                id              TEXT PRIMARY KEY,
                item_json       TEXT NOT NULL
            );
        ").map_err(|e| format!("v6 migration failed: {}", e))?;
        Ok(())
    }

    /// v7: analysis cache table.
    fn migrate_v7(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
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
        ").map_err(|e| format!("v7 migration failed: {}", e))?;
        Ok(())
    }

    /// v8: drop + recreate analysis_cache with algo_version column.
    /// Invalidates all v7 cached results (algorithm was buggy).
    fn migrate_v8(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
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
        ").map_err(|e| format!("v8 migration failed: {}", e))?;
        Ok(())
    }

    /// v9: search tag cache table.
    fn migrate_v9(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
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
        ").map_err(|e| format!("v9 migration failed: {}", e))?;
        Ok(())
    }

    /// v10: add preemphasis column to analysis_cache + bump algo version.
    fn migrate_v10(&mut self) -> Result<(), String> {
        // Add column; existing rows have NULL which is fine — the algo
        // version bump means they won't be served from cache anyway.
        self.conn.execute_batch("
            ALTER TABLE analysis_cache ADD COLUMN preemphasis INTEGER;
            ALTER TABLE analysis_cache ADD COLUMN preemphasis_corr REAL;
        ").map_err(|e| format!("v10 migration failed: {}", e))?;
        Ok(())
    }

    /// v11: add preemph_corpus table for spectral scorer model storage.
    fn migrate_v11(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS preemph_corpus (
                id          INTEGER PRIMARY KEY DEFAULT 1,
                n_frames    INTEGER NOT NULL,
                n_tracks    INTEGER NOT NULL,
                mean        BLOB NOT NULL,
                covariance  BLOB NOT NULL,
                pca         BLOB NOT NULL,
                updated_at  TEXT NOT NULL
            );
        ").map_err(|e| format!("v11 migration failed: {}", e))?;
        Ok(())
    }

    /// v12: add empirical PE template column to preemph_corpus.
    fn migrate_v12(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
            ALTER TABLE preemph_corpus ADD COLUMN pe_template BLOB;
        ").map_err(|e| format!("v12 migration failed: {}", e))?;
        Ok(())
    }

    /// v13: add preemph_classifier table for trained LDA classifier storage.
    fn migrate_v13(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
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
        ").map_err(|e| format!("v13 migration failed: {}", e))?;
        Ok(())
    }

    /// v14: add preemphasis_detail column to analysis_cache.
    fn migrate_v14(&mut self) -> Result<(), String> {
        self.conn.execute_batch("
            ALTER TABLE analysis_cache ADD COLUMN preemphasis_detail TEXT;
        ").map_err(|e| format!("v14 migration failed: {}", e))?;
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
    ) -> Option<(String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> {
        let row = self.conn.query_row(
            "SELECT tag_string, title, artist, album, genre, year
             FROM search_tag_cache
             WHERE file_path = ?1 AND file_mtime = ?2 AND file_size = ?3",
            params![file_path, mtime, size as i64],
            |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            )),
        ).ok()?;

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
    const ANALYSIS_ALGO_VERSION: i32 = 23;

    /// Look up cached analysis. Returns None if not cached, stale,
    /// or computed by an older algorithm version.
    pub fn get_cached_analysis(
        &self,
        file_path: &str,
        mtime: i64,
        size: u64,
    ) -> Option<crate::tui::analyze::AnalysisResult> {
        self.conn.query_row(
            "SELECT dr_value, peak_db, rms_db, clipping_count, dc_bias,
                    actual_bit_depth, declared_bit_depth, sample_rate, channels,
                    duration_secs, lufs, true_peak_dbtp, preemphasis, preemphasis_corr,
                    preemphasis_detail
             FROM analysis_cache
             WHERE file_path = ?1 AND file_mtime = ?2 AND file_size = ?3
               AND algo_version = ?4",
            params![file_path, mtime, size as i64, Self::ANALYSIS_ALGO_VERSION],
            |row| {
                let preemph_int: Option<i32> = row.get(12)?;
                let preemphasis = match preemph_int {
                    Some(3) => Some(crate::tui::preemphasis::PreemphasisConfidence::StrongCandidate),
                    Some(2) => Some(crate::tui::preemphasis::PreemphasisConfidence::Detected),
                    Some(1) => Some(crate::tui::preemphasis::PreemphasisConfidence::Possible),
                    _ => None,
                };
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
                })
            },
        ).ok()
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
                preemphasis_detail, analyzed_at
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            params![
                file_path, mtime, size as i64, Self::ANALYSIS_ALGO_VERSION,
                r.dr_value, r.peak_db, r.rms_db, r.clipping_count as i64, r.dc_bias,
                r.actual_bit_depth, r.declared_bit_depth, r.sample_rate, r.channels,
                r.duration_secs, r.lufs, r.true_peak_dbtp,
                r.preemphasis.as_ref().and_then(|p| match p {
                    crate::tui::preemphasis::PreemphasisConfidence::Detected => Some(2i32),
                    crate::tui::preemphasis::PreemphasisConfidence::StrongCandidate => Some(3i32),
                    crate::tui::preemphasis::PreemphasisConfidence::Possible => Some(1i32),
                    crate::tui::preemphasis::PreemphasisConfidence::NotDetected => None,
                    crate::tui::preemphasis::PreemphasisConfidence::Indeterminate => None,
                }),
                r.preemphasis_corr,
                r.preemphasis_detail,
                chrono::Utc::now().to_rfc3339(),
            ],
        ).map_err(|e| format!("analysis cache store: {}", e))?;
        Ok(())
    }

    // ── Conversion queue ─────────────────────────────────────────

    /// Full sync: replace all queue rows with the current in-memory state.
    /// Runs in a transaction for atomicity.
    pub fn sync_queue(&self, items: &[&crate::convert::ConversionItem]) -> Result<(), String> {
        let tx = self.conn.unchecked_transaction()
            .map_err(|e| format!("queue tx begin: {}", e))?;

        tx.execute("DELETE FROM conversion_queue", [])
            .map_err(|e| format!("queue clear: {}", e))?;

        for item in items {
            let json = serde_json::to_string(item)
                .map_err(|e| format!("queue item serialize: {}", e))?;
            tx.execute(
                "INSERT INTO conversion_queue (id, item_json) VALUES (?1, ?2)",
                params![item.id, json],
            ).map_err(|e| format!("queue item insert: {}", e))?;
        }

        tx.commit().map_err(|e| format!("queue tx commit: {}", e))?;
        Ok(())
    }

    /// Load all queue items from SQLite. Returns deserialized items with
    /// path validation (same as the JSON loader).
    pub fn load_queue_items(&self) -> Vec<crate::convert::ConversionItem> {
        let mut stmt = match self.conn.prepare(
            "SELECT item_json FROM conversion_queue"
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        rows.flatten()
            .filter_map(|json| serde_json::from_str::<crate::convert::ConversionItem>(&json).ok())
            .filter(|item| {
                let path_str = item.input_path.to_string_lossy();
                if path_str.contains("..") {
                    log::warn!("Filtered queue item with suspicious path: {:?}", item.input_path);
                    return false;
                }
                if !item.input_path.exists() {
                    log::info!("Filtered queue item - file gone: {:?}", item.input_path);
                    return false;
                }
                true
            })
            .collect()
    }

    /// Check if the queue table has any rows.
    pub fn has_queue_items(&self) -> bool {
        self.conn.query_row(
            "SELECT COUNT(*) FROM conversion_queue", [], |row| row.get::<_, i64>(0)
        ).unwrap_or(0) > 0
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
        self.conn.execute(
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
                input_path, output_path, input_format, output_format,
                sample_rate, bit_depth, dither, replaygain_mode,
                source_size.map(|s| s as i64), output_size.map(|s| s as i64),
                queued_at, started_at, completed_at,
                success as i32, error_message,
            ],
        ).map_err(|e| format!("history insert: {}", e))?;
        Ok(())
    }

    /// Check if a file (by path) has been successfully converted before.
    /// For dedup warnings.
    pub fn was_previously_converted(
        &self,
        input_path: &str,
    ) -> bool {
        self.conn.query_row(
            "SELECT COUNT(*) FROM conversion_history
             WHERE input_path = ?1 AND success = 1",
            params![input_path],
            |row| row.get::<_, i64>(0),
        ).unwrap_or(0) > 0
    }

    // ── Presets ───────────────────────────────────────────────────

    /// List all presets grouped by format. Returns (format, Vec<name>)
    /// sorted by format then name. Instant via indexed query.
    pub fn list_presets_by_format(&self) -> Vec<(String, Vec<String>)> {
        let mut stmt = match self.conn.prepare(
            "SELECT name, format FROM presets ORDER BY format, name"
        ) {
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
        let mut stmt = match self.conn.prepare(
            "SELECT name FROM presets ORDER BY name"
        ) {
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
        self.conn.execute(
            "INSERT OR REPLACE INTO presets (
                name, format, description, sample_rate, bit_depth,
                dither, replaygain, folder_template, filename_template, merge
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                name, format, description, sample_rate, bit_depth,
                dither, replaygain, folder_template, filename_template, merge,
            ],
        ).map_err(|e| format!("preset store: {}", e))?;
        Ok(())
    }

    /// Delete a preset by name.
    pub fn delete_preset(&self, name: &str) -> Result<(), String> {
        self.conn.execute(
            "DELETE FROM presets WHERE name = ?1",
            params![name],
        ).map_err(|e| format!("preset delete: {}", e))?;
        Ok(())
    }

    /// Check if the presets table has any entries.
    pub fn has_presets(&self) -> bool {
        self.conn.query_row(
            "SELECT COUNT(*) FROM presets", [], |row| row.get::<_, i64>(0)
        ).unwrap_or(0) > 0
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
            "INSERT INTO recent_files (file_path, accessed_at, access_count)
             VALUES (?1, ?2, 1)
             ON CONFLICT(file_path) DO UPDATE SET
                accessed_at = ?2,
                access_count = access_count + 1",
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

    /// Clear all bookmarks (for full sync from in-memory state).
    pub fn clear_bookmarks(&self) -> Result<(), String> {
        self.conn.execute("DELETE FROM bookmarks", [])
            .map_err(|e| format!("bookmarks clear: {}", e))?;
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

// ── Pre-emphasis corpus model storage ──────────────────────────────

impl Database {
    /// Load the pre-emphasis corpus model from the database.
    pub fn load_preemph_corpus(&self) -> Result<crate::tui::preemphasis::corpus::CorpusModel, String> {
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
            mean[k] = f64::from_le_bytes(mean_blob[k*8..(k+1)*8].try_into().unwrap());
        }

        // Deserialize covariance (31x31 x f64 LE).
        let cov_size = NUM_BANDS * NUM_BANDS;
        if cov_blob.len() != cov_size * 8 {
            return Err("corrupt corpus covariance blob".into());
        }
        let mut covariance = vec![0.0f64; cov_size];
        for i in 0..cov_size {
            covariance[i] = f64::from_le_bytes(cov_blob[i*8..(i+1)*8].try_into().unwrap());
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
                pc[k] = f64::from_le_bytes(pca_blob[offset + k*8..offset + (k+1)*8].try_into().unwrap());
            }
            pca_components.push(pc);
        }

        // Deserialize empirical PE template if present.
        let empirical_pe_template = pe_tmpl_blob.and_then(|blob| {
            if blob.len() != NUM_BANDS * 8 { return None; }
            let mut tmpl = [0.0f64; NUM_BANDS];
            for k in 0..NUM_BANDS {
                tmpl[k] = f64::from_le_bytes(blob[k*8..(k+1)*8].try_into().ok()?);
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
    pub fn store_preemph_corpus(&self, model: &crate::tui::preemphasis::corpus::CorpusModel) -> Result<(), String> {
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
    pub fn load_preemph_classifier(&self) -> Result<crate::tui::preemphasis::scoring::LdaClassifier, String> {
        use crate::tui::preemphasis::scoring::NUM_FEATURES;

        let (weights_blob, bias, threshold, impute_blob, means_blob, stds_blob):
            (Vec<u8>, f64, f64, Vec<u8>, Vec<u8>, Vec<u8>) =
            self.conn.query_row(
                "SELECT weights, bias, threshold, feature_impute, feature_means, feature_stds
                 FROM preemph_classifier WHERE id = 1",
                [],
                |row| Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                )),
            ).map_err(|_| "no trained classifier found (run :preemph-calibrate)".to_string())?;

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
            weights[i] = f64::from_le_bytes(weights_blob[i*8..(i+1)*8].try_into().unwrap());
            feature_impute[i] = f64::from_le_bytes(impute_blob[i*8..(i+1)*8].try_into().unwrap());
            feature_means[i] = f64::from_le_bytes(means_blob[i*8..(i+1)*8].try_into().unwrap());
            feature_stds[i] = f64::from_le_bytes(stds_blob[i*8..(i+1)*8].try_into().unwrap());
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
            for &v in arr { blob.extend_from_slice(&v.to_le_bytes()); }
            blob
        };

        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
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
        ).map_err(|e| format!("store classifier: {}", e))?;
        Ok(())
    }
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
