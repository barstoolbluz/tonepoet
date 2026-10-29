//! Complete-only SQLite repository. Called from worker threads, never the TUI reducer.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use super::model::{CachedMeasurement, CompleteMeasurement, Counts, Freshness, Observation, POLICY_VERSION, AUDIO_POLICY_VERSION};

pub fn path_key(path: &Path) -> Vec<u8> {
    #[cfg(unix)] {
        use std::os::unix::ffi::OsStrExt;
        let mut key = vec![1u8];
        key.extend_from_slice(path.as_os_str().as_bytes());
        key
    }
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        let mut key = vec![2u8];
        for value in path.as_os_str().encode_wide() { key.extend_from_slice(&value.to_le_bytes()); }
        key
    }
    #[cfg(not(any(unix, windows)))] {
        let mut key = vec![3u8];
        key.extend_from_slice(path.to_string_lossy().as_bytes());
        key
    }
}

fn micros(time: SystemTime) -> Result<i64, String> {
    let value = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_micros() as i128,
        Err(error) => -(error.duration().as_micros() as i128),
    };
    i64::try_from(value).map_err(|_| "timestamp outside SQLite range".into())
}
fn from_micros(us: i64) -> Result<SystemTime, String> {
    let duration = Duration::from_micros(us.unsigned_abs());
    if us < 0 { UNIX_EPOCH.checked_sub(duration) } else { UNIX_EPOCH.checked_add(duration) }
        .ok_or_else(|| "invalid cached timestamp".into())
}
fn checked_decimal(s: String) -> Result<u128, String> {
    let v: u128 = s.parse().map_err(|_| "invalid cached count".to_owned())?;
    if v.to_string() != s { return Err("noncanonical cached count".into()); }
    Ok(v)
}

/// Atomic publication unit: local facts + exact immediate directory edges
/// and a fully postorder-completed recursive measurement. `None` for children
/// disables edge-based reuse of enormous fanout, never completion itself.
pub(super) struct SubtreeWrite {
    pub value: CompleteMeasurement,
    pub epoch: i64,
    pub local: Counts,
    pub digest: [u8; 64],
    pub root_identity: Vec<u8>,
    pub children: Option<Vec<PathBuf>>,
}

/// Write one completed subtree inside an existing transaction. Caller owns
/// atomic batch boundaries and MUST NOT call this with an unfinished result.
fn put_complete_in_transaction(tx: &Transaction<'_>, write: &SubtreeWrite, now: i64) -> Result<bool, String> {
        let value = &write.value;
        let expected_epoch = write.epoch;
        let key = path_key(&value.path);
        let started = micros(value.started)?;
        let finished = micros(value.finished)?;
        let created = value.created.map(micros).transpose()?;
        let modified = value.modified.map(micros).transpose()?;
        let current: Option<(i64, i64)> = tx.query_row(
            "SELECT invalidation_epoch,dirty FROM directory_accounting_state_v29 WHERE path_key=?1 AND policy_version=?2 AND audio_policy_version=?3",
            params![key, POLICY_VERSION, AUDIO_POLICY_VERSION], |r| Ok((r.get(0)?,r.get(1)?))
        ).optional().map_err(|e| e.to_string())?;
        if current.map_or(0, |s| s.0) != expected_epoch { return Ok(false); }
        let prior: Option<i64> = tx.query_row(
            "SELECT run_generation FROM directory_accounting_totals_v29 WHERE path_key=?1 AND policy_version=?2 AND audio_policy_version=?3",
            params![key, POLICY_VERSION, AUDIO_POLICY_VERSION], |r| r.get(0)
        ).optional().map_err(|e| e.to_string())?;
        if prior.is_some_and(|generation| generation > value.generation) { return Ok(false); }
        tx.execute("INSERT INTO directory_accounting_state_v29(path_key,policy_version,audio_policy_version,invalidation_epoch,dirty,last_access_utc_us)\
            VALUES(?1,?2,?3,?4,0,?5) ON CONFLICT(path_key,policy_version,audio_policy_version) DO UPDATE SET dirty=0,last_access_utc_us=excluded.last_access_utc_us",
            params![key, POLICY_VERSION, AUDIO_POLICY_VERSION, expected_epoch, now]).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO directory_accounting_totals_v29(path_key,policy_version,audio_policy_version,files,descendant_folders,audio_files,logical_bytes,audio_logical_bytes,excluded_symlinks,other_entries,coverage_code,outcome_code,consistency_code,scan_started_utc_us,scan_finished_utc_us,run_generation,committed_epoch,created_utc_us,modified_utc_us,encoding_version,last_access_utc_us)\
           VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,1,?11,?12,?13,?14,?15,?16,?17,1,?18)\
           ON CONFLICT(path_key,policy_version,audio_policy_version) DO UPDATE SET\
             files=excluded.files,descendant_folders=excluded.descendant_folders,audio_files=excluded.audio_files,logical_bytes=excluded.logical_bytes,\
             audio_logical_bytes=excluded.audio_logical_bytes,excluded_symlinks=excluded.excluded_symlinks,other_entries=excluded.other_entries,\
             consistency_code=excluded.consistency_code,scan_started_utc_us=excluded.scan_started_utc_us,scan_finished_utc_us=excluded.scan_finished_utc_us,\
             run_generation=excluded.run_generation,committed_epoch=excluded.committed_epoch,created_utc_us=excluded.created_utc_us,\
             modified_utc_us=excluded.modified_utc_us,last_access_utc_us=excluded.last_access_utc_us",
           params![key, POLICY_VERSION, AUDIO_POLICY_VERSION, value.counts.files.to_string(), value.counts.folders.to_string(),
             value.counts.audio.to_string(), value.counts.bytes.to_string(), value.counts.audio_bytes.to_string(),
             value.counts.excluded_links.to_string(), value.counts.other.to_string(),
             if value.observation == Observation::AsObserved { 1 } else { 2 }, started, finished, value.generation,
             expected_epoch, created, modified, now]).map_err(|e| e.to_string())?;
        // A parent points to a new edge generation only after the entire
        // completed summary, local facts and exact child set are committed.
        let edges = write.children.as_ref();
        let edge_generation = edges.map(|_| value.generation.to_string());
        tx.execute("INSERT INTO directory_accounting_local_v29(                path_key,policy_version,audio_policy_version,direct_files,direct_folders,direct_audio,direct_logical_bytes,excluded_symlinks,                observed_at_utc_us,run_generation,edge_generation,encoding_version,direct_audio_logical_bytes,direct_other_entries,entry_digest,root_identity)            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,?12,?13,?14,?15)            ON CONFLICT(path_key,policy_version,audio_policy_version) DO UPDATE SET                direct_files=excluded.direct_files,direct_folders=excluded.direct_folders,direct_audio=excluded.direct_audio,                direct_logical_bytes=excluded.direct_logical_bytes,excluded_symlinks=excluded.excluded_symlinks,                observed_at_utc_us=excluded.observed_at_utc_us,run_generation=excluded.run_generation,                edge_generation=excluded.edge_generation,direct_audio_logical_bytes=excluded.direct_audio_logical_bytes,                direct_other_entries=excluded.direct_other_entries,entry_digest=excluded.entry_digest,root_identity=excluded.root_identity",
            params![key, POLICY_VERSION, AUDIO_POLICY_VERSION, write.local.files.to_string(), write.local.folders.to_string(),
                write.local.audio.to_string(), write.local.bytes.to_string(), write.local.excluded_links.to_string(),
                finished, value.generation, edge_generation.as_deref().unwrap_or("-"), write.local.audio_bytes.to_string(),
                write.local.other.to_string(), &write.digest[..], &write.root_identity]).map_err(|e| e.to_string())?;
        if let Some(children) = edges {
            for child in children {
                tx.execute("INSERT OR IGNORE INTO directory_accounting_edges_v29(                        parent_key,child_key,policy_version,audio_policy_version,edge_generation)                    VALUES(?1,?2,?3,?4,?5)",
                    params![key, path_key(child), POLICY_VERSION, AUDIO_POLICY_VERSION,
                        edge_generation.as_deref().unwrap_or("-")]).map_err(|e| e.to_string())?;
            }
        }
        tx.execute("UPDATE directory_accounting_state_v29 SET active_edge_generation=?1 WHERE path_key=?2            AND policy_version=?3 AND audio_policy_version=?4",
            params![edge_generation, key, POLICY_VERSION, AUDIO_POLICY_VERSION]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM directory_accounting_edges_v29 WHERE parent_key=?1            AND policy_version=?2 AND audio_policy_version=?3 AND edge_generation<>?4",
            params![key, POLICY_VERSION, AUDIO_POLICY_VERSION, edge_generation.as_deref().unwrap_or("-")])
            .map_err(|e| e.to_string())?;
        Ok(true)
}

pub struct Repository { conn: Connection }
impl Repository {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.busy_timeout(Duration::from_millis(750)).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA foreign_keys = ON;").map_err(|e| e.to_string())?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(|e| e.to_string())?;
        if version < 30 { return Err("directory accounting requires database migration v30".into()); }
        Ok(Self { conn })
    }
    pub fn allocate_generation(&mut self) -> Result<i64, String> {
        let transaction = self.conn.transaction().map_err(|e| e.to_string())?;
        let next: i64 = transaction.query_row("SELECT next_generation FROM directory_accounting_generation_v29 WHERE singleton_id=1", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        let following = next.checked_add(1).ok_or("scan generation exhausted")?;
        transaction.execute("UPDATE directory_accounting_generation_v29 SET next_generation=?1 WHERE singleton_id=1", params![following]).map_err(|e| e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(next)
    }
    pub fn mutation_sequence(&self) -> Result<i64, String> {
        self.conn.query_row("SELECT mutation_sequence FROM directory_accounting_generation_v29 WHERE singleton_id=1",
            [], |row| row.get(0)).map_err(|e| e.to_string())
    }

    /// Compose a parent from independently completed child rows, only after
    /// the caller has re-enumerated EVERY descendant in this run and matched
    /// its local facts, native identity, edge set and observed total. This is not
    /// an O(1) freshness check. Without continuous qualified watchers, do not
    /// skip filesystem traversal merely because a cached child exists.
    pub fn qualified_counts(&mut self, path: &Path, local: &Counts,
        digest: &[u8;64], identity: &[u8], fully_observed: &Counts,
    ) -> Result<Option<Counts>, String> {
        if local.folders > 256 { return Ok(None); }
        let key = path_key(path);
        let facts = self.conn.query_row(
            "SELECT direct_files,direct_folders,direct_audio,direct_logical_bytes,direct_audio_logical_bytes,                    excluded_symlinks,direct_other_entries,entry_digest,root_identity             FROM directory_accounting_local_v29 WHERE path_key=?1 AND policy_version=?2 AND audio_policy_version=?3",
            params![key, POLICY_VERSION, AUDIO_POLICY_VERSION], |r| {
                Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,
                    r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,
                    r.get::<_,String>(6)?,r.get::<_,Option<Vec<u8>>>(7)?,r.get::<_,Option<Vec<u8>>>(8)?))
            }).optional().map_err(|e| e.to_string())?;
        let Some((files,folders,audio,bytes,audio_bytes,links,other,old_digest,old_identity)) = facts else { return Ok(None); };
        if old_digest.as_deref() != Some(digest.as_slice()) || old_identity.as_deref() != Some(identity) { return Ok(None); }
        let old_local = Counts { files: checked_decimal(files)?, folders: checked_decimal(folders)?,
            audio: checked_decimal(audio)?, bytes: checked_decimal(bytes)?, audio_bytes: checked_decimal(audio_bytes)?,
            excluded_links: checked_decimal(links)?, other: checked_decimal(other)? };
        if old_local != *local { return Ok(None); }
        let edge_generation: Option<String> = self.conn.query_row(
            "SELECT active_edge_generation FROM directory_accounting_state_v29 WHERE path_key=?1 AND policy_version=?2 AND audio_policy_version=?3",
            params![key, POLICY_VERSION, AUDIO_POLICY_VERSION], |r| r.get(0))
            .optional().map_err(|e| e.to_string())?.flatten();
        let Some(edge_generation) = edge_generation else { return Ok(None); };
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM directory_accounting_edges_v29                WHERE parent_key=?1 AND policy_version=?2 AND audio_policy_version=?3 AND edge_generation=?4",
            params![key, POLICY_VERSION, AUDIO_POLICY_VERSION, edge_generation], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if count as u128 != local.folders { return Ok(None); }
        // Aggregate the *completed child rows*, not the old parent row. The
        // calling walker has independently enumerated this entire subtree;
        // that observation is essential evidence on systems without qualified
        // continuous notifications. Historical child totals alone prove no
        // freshness, even when their root mtime is unchanged.
        let mut total = *local;
        let mut observed_children = 0u128;
        let mut statement = self.conn.prepare(
            "SELECT t.files,t.descendant_folders,t.audio_files,t.logical_bytes,
                    t.audio_logical_bytes,t.excluded_symlinks,t.other_entries
               FROM directory_accounting_edges_v29 e
               JOIN directory_accounting_totals_v29 t
                 ON t.path_key=e.child_key AND t.policy_version=e.policy_version
                AND t.audio_policy_version=e.audio_policy_version
              WHERE e.parent_key=?1 AND e.policy_version=?2
                AND e.audio_policy_version=?3 AND e.edge_generation=?4
                AND t.coverage_code=1 AND t.outcome_code=1 AND t.encoding_version=1")
            .map_err(|e| e.to_string())?;
        let child_rows = statement.query_map(
            params![key, POLICY_VERSION, AUDIO_POLICY_VERSION, edge_generation],
            |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,
                row.get::<_,String>(2)?,row.get::<_,String>(3)?,
                row.get::<_,String>(4)?,row.get::<_,String>(5)?,
                row.get::<_,String>(6)?)))
            .map_err(|e| e.to_string())?;
        for row in child_rows {
            let (files,folders,audio,bytes,audio_bytes,links,other) = row.map_err(|e| e.to_string())?;
            let child = Counts {
                files: checked_decimal(files)?, folders: checked_decimal(folders)?,
                audio: checked_decimal(audio)?, bytes: checked_decimal(bytes)?,
                audio_bytes: checked_decimal(audio_bytes)?,
                excluded_links: checked_decimal(links)?, other: checked_decimal(other)?,
            };
            total.merge(&child).map_err(str::to_owned)?;
            observed_children = observed_children.checked_add(1)
                .ok_or_else(|| "directory edge count overflow".to_string())?;
        }
        if observed_children != local.folders || total != *fully_observed {
            return Ok(None);
        }
        Ok(Some(total))
    }

    pub fn epoch(&self, path: &Path) -> Result<i64, String> {
        self.conn.query_row(
            "SELECT invalidation_epoch FROM directory_accounting_state_v29 WHERE path_key=?1 AND policy_version=?2 AND audio_policy_version=?3",
            params![path_key(path), POLICY_VERSION, AUDIO_POLICY_VERSION], |r| r.get(0)
        ).optional().map(|v| v.unwrap_or(0)).map_err(|e| e.to_string())
    }
    pub fn lookup(&mut self, path: &Path) -> Result<Option<CachedMeasurement>, String> {
        let values = self.conn.query_row(
            "SELECT t.files,t.descendant_folders,t.audio_files,t.logical_bytes,t.audio_logical_bytes,t.excluded_symlinks,t.other_entries,\
                    t.scan_started_utc_us,t.scan_finished_utc_us,t.run_generation,t.consistency_code,s.dirty,s.invalidation_epoch,t.committed_epoch,\
                    t.created_utc_us,t.modified_utc_us,t.last_access_utc_us\
             FROM directory_accounting_totals_v29 t LEFT JOIN directory_accounting_state_v29 s\
             ON s.path_key=t.path_key AND s.policy_version=t.policy_version AND s.audio_policy_version=t.audio_policy_version\
             WHERE t.path_key=?1 AND t.policy_version=?2 AND t.audio_policy_version=?3 AND t.coverage_code=1 AND t.outcome_code=1 AND t.encoding_version=1",
            params![path_key(path), POLICY_VERSION, AUDIO_POLICY_VERSION], |r| {
                Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,i64>(7)?,r.get::<_,i64>(8)?,r.get::<_,i64>(9)?,r.get::<_,i64>(10)?,r.get::<_,Option<i64>>(11)?,r.get::<_,Option<i64>>(12)?,r.get::<_,i64>(13)?,r.get::<_,Option<i64>>(14)?,r.get::<_,Option<i64>>(15)?,r.get::<_,i64>(16)?))
            }
        ).optional().map_err(|e| e.to_string())?;
        let Some((files,folders,audio,bytes,audio_bytes,links,other,started,finished,generation,observation,dirty,epoch,committed,created,modified,last_access)) = values else { return Ok(None); };
        // One LRU touch per day at most, not one write per navigation.
        if let Ok(now) = micros(SystemTime::now()) {
            if now.saturating_sub(last_access) > 86_400_000_000i64 {
                let _ = self.conn.execute("UPDATE directory_accounting_totals_v29 SET last_access_utc_us=?1 WHERE path_key=?2 AND policy_version=?3 AND audio_policy_version=?4",
                    params![now, path_key(path), POLICY_VERSION, AUDIO_POLICY_VERSION]);
            }
        }
        let counts = Counts { files: checked_decimal(files)?, folders: checked_decimal(folders)?, audio: checked_decimal(audio)?, bytes: checked_decimal(bytes)?, audio_bytes: checked_decimal(audio_bytes)?, excluded_links: checked_decimal(links)?, other: checked_decimal(other)? };
        let value = CompleteMeasurement {
            path: path.to_path_buf(), counts, started: from_micros(started)?, finished: from_micros(finished)?,
            created: created.map(from_micros).transpose()?, modified: modified.map(from_micros).transpose()?,
            generation, observation: if observation == 1 { Observation::AsObserved } else { Observation::Indeterminate },
        };
        let freshness = if dirty != Some(0) || epoch.unwrap_or(0) != committed { Freshness::KnownInvalid } else { Freshness::Unverified };
        Ok(Some(CachedMeasurement { measurement: value, freshness }))
    }
    /// All-or-nothing short transaction. The global mutation sequence fences
    /// writes into *previously uncached* directories, where no per-path
    /// invalidation epoch row existed when traversal began.
    pub fn put_batch_at_sequence(&mut self, values: &[SubtreeWrite], expected_sequence: i64) -> Result<Vec<bool>, String> {
        if values.len() > 32 { return Err("directory accounting batch exceeds 32 subtrees".into()); }
        let now = micros(SystemTime::now())?;
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        let current: i64 = tx.query_row("SELECT mutation_sequence FROM directory_accounting_generation_v29 WHERE singleton_id=1",
            [], |row| row.get(0)).map_err(|e| e.to_string())?;
        if current != expected_sequence { return Ok(vec![false; values.len()]); }
        let mut outcomes = Vec::with_capacity(values.len());
        for write in values { outcomes.push(put_complete_in_transaction(&tx, write, now)?); }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(outcomes)
    }
    #[cfg(test)]
    pub fn put_complete(&mut self, value: &CompleteMeasurement, expected_epoch: i64) -> Result<bool, String> {
        let sequence = self.mutation_sequence()?;
        let write = SubtreeWrite { value: value.clone(), epoch: expected_epoch, local: value.counts,
            digest: [0;64], root_identity: Vec::new(), children: None };
        self.put_batch_at_sequence(&[write], sequence).map(|results| results[0])
    }
    /// Fail-closed recovery when the bounded invalidation channel overflows.
    /// Runs only on the SQLite worker; no filesystem or DB I/O on the TUI.
    pub fn mark_all_dirty(&mut self) -> Result<(), String> {
        let now = micros(SystemTime::now())?;
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        let advanced = tx.execute("UPDATE directory_accounting_generation_v29
             SET mutation_sequence=mutation_sequence+1
             WHERE singleton_id=1 AND mutation_sequence<9223372036854775807", [])
            .map_err(|e| e.to_string())?;
        if advanced != 1 { return Err("directory invalidation sequence exhausted".into()); }
        tx.execute("UPDATE directory_accounting_state_v29 SET
            invalidation_epoch=invalidation_epoch+1,dirty=1,dirty_at_utc_us=?1
            WHERE invalidation_epoch<9223372036854775807", params![now])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn mark_dirty(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        if paths.is_empty() { return Ok(()); }
        let now = micros(SystemTime::now())?;
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        // Durable global fence for scan-versus-mutation races, even before
        // the first completed row for a directory is ever published.
        let advanced = tx.execute("UPDATE directory_accounting_generation_v29 SET mutation_sequence=mutation_sequence+1
            WHERE singleton_id=1 AND mutation_sequence<9223372036854775807", [])
            .map_err(|e| e.to_string())?;
        if advanced != 1 { return Err("directory invalidation sequence exhausted".into()); }
        let mut unique = std::collections::HashSet::new();
        for path in paths {
            for ancestor in path.ancestors() {
                let key = path_key(ancestor);
                if !unique.insert(key.clone()) { continue; }
                // Only tracked, completed directories have state. No row is
                // created for the mutated file or an unindexed ancestor.
                tx.execute("UPDATE directory_accounting_state_v29 SET                    invalidation_epoch=invalidation_epoch+1,dirty=1,dirty_at_utc_us=?4                    WHERE path_key=?1 AND policy_version=?2 AND audio_policy_version=?3",
                    params![key, POLICY_VERSION, AUDIO_POLICY_VERSION, now]).map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn prune(&mut self) -> Result<usize, String> {
        let mut removed = 0;
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM directory_accounting_totals_v29", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if count > 250_000 {
            let limit = (count - 250_000).min(5000);
            removed += self.conn.execute("DELETE FROM directory_accounting_totals_v29 WHERE rowid IN                (SELECT rowid FROM directory_accounting_totals_v29 ORDER BY last_access_utc_us ASC LIMIT ?1)",
                params![limit]).map_err(|e| e.to_string())?;
        }
        // Repair orphaned v29 invalidation rows and bound every auxiliary
        // table. Unfinished or cancelled jobs leave no authoritative totals;
        // they cannot retain indefinite invalidation or scratch records.
        removed += self.conn.execute("DELETE FROM directory_accounting_state_v29 WHERE rowid IN            (SELECT s.rowid FROM directory_accounting_state_v29 s WHERE NOT EXISTS              (SELECT 1 FROM directory_accounting_totals_v29 t WHERE t.path_key=s.path_key               AND t.policy_version=s.policy_version AND t.audio_policy_version=s.audio_policy_version) LIMIT 5000)",
            []).map_err(|e| e.to_string())?;
        removed += self.conn.execute("DELETE FROM directory_accounting_local_v29 WHERE rowid IN            (SELECT l.rowid FROM directory_accounting_local_v29 l WHERE NOT EXISTS              (SELECT 1 FROM directory_accounting_totals_v29 t WHERE t.path_key=l.path_key               AND t.policy_version=l.policy_version AND t.audio_policy_version=l.audio_policy_version) LIMIT 5000)",
            []).map_err(|e| e.to_string())?;
        removed += self.conn.execute("DELETE FROM directory_accounting_edges_v29 WHERE rowid IN            (SELECT e.rowid FROM directory_accounting_edges_v29 e LEFT JOIN directory_accounting_state_v29 s              ON s.path_key=e.parent_key AND s.policy_version=e.policy_version AND s.audio_policy_version=e.audio_policy_version              WHERE s.active_edge_generation IS NULL OR e.edge_generation<>s.active_edge_generation LIMIT 5000)",
            []).map_err(|e| e.to_string())?;
        let edges: i64 = self.conn.query_row("SELECT COUNT(*) FROM directory_accounting_edges_v29", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if edges > 1_000_000 {
            let tx = self.conn.transaction().map_err(|e| e.to_string())?;
            let paths: Vec<Vec<u8>> = {
                let mut statement = tx.prepare("SELECT DISTINCT parent_key FROM                    (SELECT parent_key FROM directory_accounting_edges_v29 ORDER BY rowid LIMIT 512) LIMIT 32")
                    .map_err(|e| e.to_string())?;
                let rows = statement.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?
                    .collect::<Result<Vec<_>,_>>().map_err(|e| e.to_string())?;
                rows
            };
            for key in paths {
                tx.execute("UPDATE directory_accounting_state_v29 SET active_edge_generation=NULL                    WHERE path_key=?1 AND policy_version=?2 AND audio_policy_version=?3",
                    params![key, POLICY_VERSION, AUDIO_POLICY_VERSION]).map_err(|e| e.to_string())?;
                removed += tx.execute("DELETE FROM directory_accounting_edges_v29 WHERE parent_key=?1                    AND policy_version=?2 AND audio_policy_version=?3",
                    params![key, POLICY_VERSION, AUDIO_POLICY_VERSION]).map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
        Ok(removed)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    fn measurement(path: &Path, generation: i64, bytes: u128) -> CompleteMeasurement {
        let now = SystemTime::now();
        CompleteMeasurement {
            path: path.to_path_buf(), counts: Counts { bytes, files: 1, ..Counts::default() },
            started: now, finished: now, created: None, modified: None,
            generation, observation: Observation::AsObserved,
        }
    }
    #[test]
    fn completed_measurements_survive_restart_but_not_as_verified_current() {
        let work = tempfile::tempdir().unwrap();
        let db = work.path().join("accounting.db");
        let path = work.path().join("collection");
        crate::db::Database::open_path(&db).unwrap();
        {
            let mut repository = Repository::open(&db).unwrap();
            let generation = repository.allocate_generation().unwrap();
            assert!(repository.put_complete(&measurement(&path, generation, 901), 0).unwrap());
        }
        {
            let mut repository = Repository::open(&db).unwrap();
            let cached = repository.lookup(&path).unwrap().unwrap();
            assert_eq!(cached.measurement.counts.bytes, 901);
            assert_eq!(cached.freshness, Freshness::Unverified);
            repository.mark_dirty(&[path.join("deep/track.flac")]).unwrap();
            let changed = repository.lookup(&path).unwrap().unwrap();
            assert_eq!(changed.freshness, Freshness::KnownInvalid);
            assert_eq!(changed.measurement.counts.bytes, 901);
            // An old scan cannot re-publish over intervening invalidation.
            assert!(!repository.put_complete(&measurement(&path, 100, 1), 0).unwrap());
            let epoch = repository.epoch(&path).unwrap();
            let generation = repository.allocate_generation().unwrap();
            assert!(repository.put_complete(&measurement(&path, generation, 902), epoch).unwrap());
            assert_eq!(repository.lookup(&path).unwrap().unwrap().freshness, Freshness::Unverified);
        }
    }
    #[test]
    fn invalidating_many_uncached_files_keeps_state_bounded() {
        let work = tempfile::tempdir().unwrap();
        let db = work.path().join("bounded.db");
        crate::db::Database::open_path(&db).unwrap();
        let mut repo = Repository::open(&db).unwrap();
        let root = work.path().join("Collection");
        let generation = repo.allocate_generation().unwrap();
        assert!(repo.put_complete(&measurement(&root, generation, 4), 0).unwrap());
        for n in 0..1000 {
            repo.mark_dirty(&[root.join(format!("artist-{n}/album/track.flac"))]).unwrap();
        }
        let count: i64 = repo.conn.query_row("SELECT COUNT(*) FROM directory_accounting_state_v29", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "a thousand distinct mutated files cannot create state rows");
        assert_eq!(repo.mutation_sequence().unwrap(), 1000);
        assert_eq!(repo.lookup(&root).unwrap().unwrap().freshness, Freshness::KnownInvalid);
    }

    #[test]
    fn mutation_fence_rejects_scan_of_previously_uncached_directory() {
        let work = tempfile::tempdir().unwrap();
        let db = work.path().join("fence.db");
        crate::db::Database::open_path(&db).unwrap();
        let mut repo = Repository::open(&db).unwrap();
        let root = work.path().join("NewCollection");
        let before = repo.mutation_sequence().unwrap();
        let generation = repo.allocate_generation().unwrap();
        let counts = Counts { files: 1, bytes: 50, ..Counts::default() };
        let value = measurement(&root, generation, 50);
        let write = SubtreeWrite {
            value, epoch: 0, local: counts, digest: [2u8;64],
            root_identity: vec![1,2], children: Some(vec![]),
        };
        repo.mark_dirty(&[root.join("child/new.flac")]).unwrap();
        assert_eq!(repo.put_batch_at_sequence(&[write], before).unwrap(), vec![false]);
        assert!(repo.lookup(&root).unwrap().is_none(),
            "a source mutation can reject the first ever cache publication");
    }

    #[test]
    fn local_digest_and_edge_graph_gate_historical_subtree_reuse() {
        let work = tempfile::tempdir().unwrap();
        let db = work.path().join("edges.db");
        crate::db::Database::open_path(&db).unwrap();
        let mut repo = Repository::open(&db).unwrap();
        let root = work.path().join("Album");
        let generation = repo.allocate_generation().unwrap();
        let value = measurement(&root, generation, 901);
        let local = value.counts;
        let digest = [5u8;64];
        let identity = b"valid-directory-id".to_vec();
        let write = SubtreeWrite {
            value, epoch: 0, local, digest, root_identity: identity.clone(),
            children: Some(vec![]),
        };
        let seq = repo.mutation_sequence().unwrap();
        assert_eq!(repo.put_batch_at_sequence(&[write], seq).unwrap(), vec![true]);
        assert_eq!(repo.qualified_counts(&root, &local, &digest, &identity, &local).unwrap(), Some(local));
        assert_eq!(repo.qualified_counts(&root, &local, &[7u8;64], &identity, &local).unwrap(), None);
        let altered = Counts { bytes: 902, ..local };
        assert_eq!(repo.qualified_counts(&root, &local, &digest, &identity, &altered).unwrap(), None);
        assert_eq!(repo.qualified_counts(&root, &local, &digest, b"replaced-object", &local).unwrap(), None);
    }

    #[test]
    fn completed_child_summaries_are_summed_into_qualified_parent() {
        let work = tempfile::tempdir().unwrap();
        let db = work.path().join("bottom_up.db");
        crate::db::Database::open_path(&db).unwrap();
        let mut repo = Repository::open(&db).unwrap();
        let parent = work.path().join("collection");
        let child = parent.join("album");
        let child_generation = repo.allocate_generation().unwrap();
        let child_value = measurement(&child, child_generation, 4096);
        let parent_generation = repo.allocate_generation().unwrap();
        let mut parent_value = measurement(&parent, parent_generation, 4096);
        parent_value.counts.folders = 1;
        let parent_direct = Counts { folders: 1, ..Counts::default() };
        let parent_digest = [11u8;64];
        let parent_id = vec![4,2];
        let seq = repo.mutation_sequence().unwrap();
        let writes = [
            SubtreeWrite { value: child_value.clone(), epoch: 0, local: child_value.counts,
                digest: [3;64], root_identity: vec![3], children: Some(vec![]) },
            SubtreeWrite { value: parent_value.clone(), epoch: 0, local: parent_direct,
                digest: parent_digest, root_identity: parent_id.clone(),
                children: Some(vec![child.clone()]) },
        ];
        assert_eq!(repo.put_batch_at_sequence(&writes, seq).unwrap(), [true,true]);
        assert_eq!(repo.qualified_counts(&parent, &parent_direct,
            &parent_digest, &parent_id, &parent_value.counts).unwrap(), Some(parent_value.counts));
        // If retention evicts a dependent child, never synthesize a fresh
        // parent from an incomplete edge graph.
        repo.conn.execute("DELETE FROM directory_accounting_totals_v29 WHERE path_key=?1",
            params![path_key(&child)]).unwrap();
        assert_eq!(repo.qualified_counts(&parent, &parent_direct,
            &parent_digest, &parent_id, &parent_value.counts).unwrap(), None);
    }

    #[test]
    fn failed_batch_rolls_back_completed_rows_and_local_facts() {
        let work = tempfile::tempdir().unwrap();
        let db = work.path().join("atomic.db");
        crate::db::Database::open_path(&db).unwrap();
        let mut repo = Repository::open(&db).unwrap();
        let root = work.path().join("Root");
        let child = work.path().join("Bad");
        let generation = repo.allocate_generation().unwrap();
        let good = measurement(&root, generation, 100);
        let mut bad = measurement(&child, generation, 20);
        bad.finished = UNIX_EPOCH; // violates SQLite CHECK after the first write
        let writes = [
            SubtreeWrite { value: good.clone(), epoch: 0, local: good.counts,
                digest: [0;64], root_identity: vec![], children: Some(vec![]) },
            SubtreeWrite { value: bad.clone(), epoch: 0, local: bad.counts,
                digest: [0;64], root_identity: vec![], children: Some(vec![]) },
        ];
        assert!(repo.put_batch_at_sequence(&writes, repo.mutation_sequence().unwrap()).is_err());
        assert!(repo.lookup(&root).unwrap().is_none());
        let facts: i64 = repo.conn.query_row("SELECT COUNT(*) FROM directory_accounting_local_v29",[],|r|r.get(0)).unwrap();
        assert_eq!(facts, 0);
    }

    #[test]
    fn overflow_recovery_marks_entire_historical_cache_invalid() {
        let work = tempfile::tempdir().unwrap();
        let db = work.path().join("overflow.db");
        crate::db::Database::open_path(&db).unwrap();
        let mut repo = Repository::open(&db).unwrap();
        let roots = [work.path().join("A"), work.path().join("B")];
        for root in &roots {
            let generation = repo.allocate_generation().unwrap();
            assert!(repo.put_complete(&measurement(root, generation, 3), 0).unwrap());
        }
        repo.mark_all_dirty().unwrap();
        for root in &roots {
            assert_eq!(repo.lookup(root).unwrap().unwrap().freshness, Freshness::KnownInvalid);
        }
        assert_eq!(repo.mutation_sequence().unwrap(), 1);
    }

    #[test]
    fn missing_or_unmigrated_database_is_not_treated_as_an_empty_cache() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("unmigrated.db");
        assert!(Repository::open(&db).is_err());
    }
}
